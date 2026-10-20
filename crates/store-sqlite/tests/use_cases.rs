//! The use cases over the SQLite store on a real file: races between many sessions on a
//! multi-threaded runtime, each with exactly one winner, and two projects in one file that never see
//! each other's records.

#[cfg(test)]
mod use_cases {
    use std::sync::Arc;

    use tempfile::TempDir;
    use time::{Duration, OffsetDateTime};

    use dibs_app::fakes::Fakes;
    use dibs_app::named::Refusal;
    use dibs_app::ports::Store;
    use dibs_app::use_cases::{
        Addressees, ClaimItemInput, ClaimMade, CreateItemInput, CreateMemberInput, DeliveryAsk,
        EventCursor, HoldMode, InitProjectInput, ItemQuery, NewMember, RegisterSessionInput,
        ReleaseSlotInput, StartThreadInput, TakeOverInput, TakeSlotInput, TookKind, TookSlot,
        UseCases,
    };
    use dibs_app::{Actor, AppError, Dependencies};
    use dibs_domain::claim::{ClaimOutcome, ClaimRefused, TakeoverRefused};
    use dibs_domain::handle::Handle;
    use dibs_domain::ids::{ItemId, MemberId, ProjectId, SessionId};
    use dibs_domain::item::{ExternalRef, Title};
    use dibs_domain::member::{DisplayName, Role};
    use dibs_domain::message::{Body, SessionLabel, Subject};
    use dibs_domain::project::ProjectSlug;
    use dibs_domain::session::{AgentLabel, HostName};
    use dibs_domain::settings::SettingsDocument;
    use dibs_domain::slot::{Purpose, SlotName};
    use dibs_store_sqlite::SqliteStore;

    fn unread_only(unread_only: bool) -> dibs_app::use_cases::InboxQuery {
        dibs_app::use_cases::InboxQuery {
            unread_only,
            unacked: false,
            deliverable: false,
        }
    }

    const RACERS: usize = 32;

    /// The use cases over a fresh database file, with the fakes for every other port.
    struct World {
        fakes: Fakes,
        store: Arc<SqliteStore>,
        app: UseCases,
        _dir: TempDir,
    }

    impl World {
        async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(
                SqliteStore::open(&dir.path().join("dibs.db"))
                    .await
                    .unwrap(),
            );
            let fakes = Fakes::new(OffsetDateTime::UNIX_EPOCH);
            let deps = Dependencies {
                store: store.clone(),
                ..fakes.dependencies()
            };
            Self {
                app: UseCases::new(&deps),
                fakes,
                store,
                _dir: dir,
            }
        }

        /// The names of every event of the project, oldest first.
        async fn event_names(&self, project: ProjectId) -> Vec<&'static str> {
            let mut tx = self.store.begin().await.unwrap();
            tx.events_after(project, None, 10_000)
                .await
                .unwrap()
                .iter()
                .map(|event| event.kind.name())
                .collect()
        }
    }

    fn member(handle: &str) -> NewMember {
        NewMember {
            handle: Handle::parse(handle).unwrap(),
            display_name: DisplayName::parse(handle).unwrap(),
            external_login: None,
        }
    }

    /// A new project with its admin, signed in.
    async fn project(app: &UseCases, slug: &str, admin: &str) -> Actor {
        let created = app
            .init_project
            .execute(InitProjectInput {
                slug: ProjectSlug::parse(slug).unwrap(),
                admin: member(admin),
            })
            .await
            .unwrap();
        app.authenticate
            .execute(created.token.expose())
            .await
            .unwrap()
    }

    async fn register(app: &UseCases, actor: &Actor, n: usize) -> SessionId {
        app.register_session
            .execute(
                actor,
                RegisterSessionInput {
                    host: HostName::parse(&format!("host-{n}")).unwrap(),
                    label: SessionLabel::parse(&format!("host-{n}/mcp")).unwrap(),
                    agent: AgentLabel::parse("agent").unwrap(),
                    push: false,
                },
            )
            .await
            .unwrap()
            .id
    }

    async fn create_item(app: &UseCases, actor: &Actor, n: usize) -> ItemId {
        app.create_item
            .execute(
                actor,
                CreateItemInput {
                    external_ref: Some(
                        ExternalRef::parse(&format!("gh:acme/widgets#{n}")).unwrap(),
                    ),
                    title: Title::parse("Contested").unwrap(),
                },
            )
            .await
            .unwrap()
            .item
            .item
            .id
    }

    struct Racer {
        actor: Actor,
        session: SessionId,
    }

    /// A project with one area and many members, each with a session of their own.
    struct Race {
        world: World,
        admin: Actor,
        racers: Vec<Racer>,
    }

    impl Race {
        async fn new() -> Arc<Self> {
            let world = World::new().await;
            let app = &world.app;
            let admin = project(app, "widgets", "admin").await;
            let document: SettingsDocument = serde_json::from_str(
                r#"{"areas":[{"name":"billing","paths":["billing/**"]}],
                        "slots":[{"name":"deploy:prod","max_hold":"30m"}]}"#,
            )
            .unwrap();
            app.apply_settings.execute(&admin, document).await.unwrap();
            let mut racers = Vec::new();
            for n in 0..RACERS {
                let token = app
                    .create_member
                    .execute(
                        &admin,
                        CreateMemberInput {
                            member: member(&format!("racer-{n}")),
                            role: Role::Member,
                        },
                    )
                    .await
                    .unwrap()
                    .token;
                let actor = app.authenticate.execute(token.expose()).await.unwrap();
                let session = register(app, &actor, n).await;
                racers.push(Racer { actor, session });
            }
            Arc::new(Self {
                world,
                admin,
                racers,
            })
        }

        /// Every racer claims at once: racer `n` claims `items[n]` declaring `areas`.
        async fn claim_all(
            self: &Arc<Self>,
            items: &[ItemId],
            areas: &[&str],
        ) -> Vec<(MemberId, Result<ClaimMade, AppError>)> {
            let mut tasks = Vec::new();
            for (n, item) in items.iter().copied().enumerate() {
                let race = self.clone();
                let input = ClaimItemInput {
                    wide: None,
                    item,
                    session: race.racers[n].session,
                    areas: areas.iter().map(|a| (*a).to_owned()).collect(),
                };
                tasks.push(tokio::spawn(async move {
                    let racer = &race.racers[n];
                    (
                        racer.actor.member,
                        race.world.app.claim_item.execute(&racer.actor, input).await,
                    )
                }));
            }
            let mut results = Vec::new();
            for task in tasks {
                results.push(task.await.unwrap());
            }
            results
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn one_item_has_one_winner_and_every_loser_learns_who() {
        let race = Race::new().await;
        let item = create_item(&race.world.app, &race.admin, 1).await;
        let results = race.claim_all(&[item; RACERS], &[]).await;
        let winners: Vec<MemberId> = results
            .iter()
            .filter(|(_, r)| {
                matches!(
                    r,
                    Ok(ClaimMade {
                        outcome: ClaimOutcome::New { .. },
                        ..
                    })
                )
            })
            .map(|(member, _)| *member)
            .collect();
        assert_eq!(winners.len(), 1, "{results:?}");
        let winner = winners[0];
        for (member, result) in &results {
            if *member != winner {
                assert!(
                    matches!(
                        result,
                        Err(AppError::Claim(Refusal {
                            reason: ClaimRefused::HeldBy { member: holder, .. },
                            member: Some(_),
                            ..
                        })) if *holder == winner
                    ),
                    "{result:?}"
                );
            }
        }
        let view = race
            .world
            .app
            .get_item
            .execute(&race.admin, item)
            .await
            .unwrap();
        assert_eq!(view.claim.map(|c| c.claim.member), Some(winner));
        let names = race.world.event_names(race.admin.project).await;
        assert_eq!(names.iter().filter(|n| **n == "claimed").count(), 1);
    }

    /// Racers `0..count` take the slot at once.
    async fn take_all(
        race: &Arc<Race>,
        count: usize,
    ) -> Vec<(MemberId, Result<TookSlot, AppError>)> {
        let mut tasks = Vec::new();
        for n in 0..count {
            let race = race.clone();
            tasks.push(tokio::spawn(async move {
                let racer = &race.racers[n];
                let input = TakeSlotInput {
                    slot: SlotName::parse("deploy:prod").unwrap(),
                    session: racer.session,
                    purpose: Purpose::parse("ship it").unwrap(),
                    item: None,
                    mode: HoldMode::Session,
                };
                (
                    racer.actor.member,
                    race.world.app.take_slot.execute(&racer.actor, input).await,
                )
            }));
        }
        let mut results = Vec::new();
        for task in tasks {
            results.push(task.await.unwrap());
        }
        results
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn many_takers_of_one_slot_make_one_holder_and_a_queue_in_order() {
        let race = Race::new().await;
        let results = take_all(&race, RACERS).await;
        let holders: Vec<MemberId> = results
            .iter()
            .filter(|(_, r)| {
                matches!(
                    r,
                    Ok(TookSlot {
                        kind: TookKind::Taken,
                        ..
                    })
                )
            })
            .map(|(member, _)| *member)
            .collect();
        assert_eq!(holders.len(), 1, "{results:?}");
        let mut places: Vec<(usize, MemberId)> = results
            .iter()
            .filter_map(|(member, r)| match r {
                Ok(TookSlot {
                    kind: TookKind::Queued { position },
                    ..
                }) => Some((*position, *member)),
                _ => None,
            })
            .collect();
        places.sort();
        let positions: Vec<usize> = places.iter().map(|(p, _)| *p).collect();
        assert_eq!(
            positions,
            (1..RACERS).collect::<Vec<_>>(),
            "each place once"
        );
        let line = race
            .world
            .app
            .slots
            .one(&race.admin, &SlotName::parse("deploy:prod").unwrap())
            .await
            .unwrap();
        assert_eq!(line.hold.map(|h| h.hold.member), Some(holders[0]));
        let queue: Vec<MemberId> = line.queue.iter().map(|w| w.waiter.member).collect();
        let answered: Vec<MemberId> = places.iter().map(|(_, m)| *m).collect();
        assert_eq!(
            queue, answered,
            "the queue is in the order the places were answered"
        );
        let names = race.world.event_names(race.admin.project).await;
        assert_eq!(names.iter().filter(|n| **n == "slot_taken").count(), 1);
        assert_eq!(
            names.iter().filter(|n| **n == "slot_queued").count(),
            RACERS - 1
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn two_takers_race_a_hundred_rounds_and_each_round_has_one_holder_and_one_waiter() {
        let race = Race::new().await;
        let slot = SlotName::parse("deploy:prod").unwrap();
        for round in 0..100 {
            let results = take_all(&race, 2).await;
            let taken = results
                .iter()
                .filter(|(_, r)| {
                    matches!(
                        r,
                        Ok(TookSlot {
                            kind: TookKind::Taken,
                            ..
                        })
                    )
                })
                .count();
            let queued = results
                .iter()
                .filter(|(_, r)| {
                    matches!(
                        r,
                        Ok(TookSlot {
                            kind: TookKind::Queued { position: 1 },
                            ..
                        })
                    )
                })
                .count();
            assert_eq!((taken, queued), (1, 1), "round {round}: {results:?}");
            for (n, (_, result)) in results.iter().enumerate() {
                let racer = &race.racers[n];
                let release = ReleaseSlotInput {
                    slot: slot.clone(),
                    run: None,
                    exit: None,
                };
                match result {
                    Ok(TookSlot {
                        kind: TookKind::Taken,
                        ..
                    }) => {
                        race.world
                            .app
                            .release_slot
                            .execute(&racer.actor, release)
                            .await
                            .unwrap();
                    }
                    _ => {
                        race.world
                            .app
                            .leave_slot_queue
                            .execute(&racer.actor, &slot)
                            .await
                            .unwrap();
                    }
                }
            }
            let line = race.world.app.slots.one(&race.admin, &slot).await.unwrap();
            assert!(
                line.hold.is_none() && line.queue.is_empty() && line.offer.is_none(),
                "round {round}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn one_area_has_one_holder_across_items() {
        let race = Race::new().await;
        let mut items = Vec::new();
        for n in 0..RACERS {
            items.push(create_item(&race.world.app, &race.admin, n).await);
        }
        let results = race.claim_all(&items, &["billing"]).await;
        let winners: Vec<MemberId> = results
            .iter()
            .filter(|(_, r)| r.is_ok())
            .map(|(member, _)| *member)
            .collect();
        assert_eq!(winners.len(), 1, "{results:?}");
        for (member, result) in &results {
            if *member != winners[0] {
                assert!(
                    matches!(
                        result,
                        Err(AppError::Claim(Refusal {
                            reason: ClaimRefused::AreaHeld { member: holder, .. },
                            member: Some(_),
                            ..
                        })) if *holder == winners[0]
                    ),
                    "{result:?}"
                );
            }
        }
        let status = race.world.app.status.execute(&race.admin).await.unwrap();
        assert_eq!(status.holds.len(), 1);
        assert_eq!(status.holds[0].member, winners[0]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn a_stale_claim_is_taken_over_once() {
        let race = Race::new().await;
        let app = &race.world.app;
        let item = create_item(app, &race.admin, 1).await;
        app.claim_item
            .execute(
                &race.racers[0].actor,
                ClaimItemInput {
                    wide: None,
                    item,
                    session: race.racers[0].session,
                    areas: Vec::new(),
                },
            )
            .await
            .unwrap();
        race.world.fakes.clock.advance(Duration::hours(25));
        let mut tasks = Vec::new();
        for n in 1..RACERS {
            let race = race.clone();
            tasks.push(tokio::spawn(async move {
                let racer = &race.racers[n];
                let input = TakeOverInput {
                    item,
                    session: racer.session,
                    areas: Vec::new(),
                    reason: Body::parse("Picking this up.").unwrap(),
                };
                race.world.app.take_over.execute(&racer.actor, input).await
            }));
        }
        let mut won = 0;
        for task in tasks {
            match task.await.unwrap() {
                Ok(_) => won += 1,
                Err(error) => assert!(
                    matches!(
                        error,
                        AppError::Takeover(Refusal {
                            reason: TakeoverRefused::NotStale { .. },
                            ..
                        })
                    ),
                    "{error:?}"
                ),
            }
        }
        assert_eq!(won, 1);
        let names = race.world.event_names(race.admin.project).await;
        assert_eq!(names.iter().filter(|n| **n == "taken_over").count(), 1);
    }

    #[tokio::test]
    async fn a_member_of_one_project_never_reaches_another_s_records() {
        let world = World::new().await;
        let app = &world.app;
        let alice = project(app, "widgets", "alice").await;
        let dave = project(app, "gadgets", "dave").await;
        assert_ne!(alice.project, dave.project);
        let alices_session = register(app, &alice, 1).await;
        let daves_session = register(app, &dave, 2).await;

        let item = create_item(app, &alice, 7).await;
        app.claim_item
            .execute(
                &alice,
                ClaimItemInput {
                    wide: None,
                    item,
                    session: alices_session,
                    areas: Vec::new(),
                },
            )
            .await
            .unwrap();
        let thread = app
            .start_thread
            .execute(
                &alice,
                StartThreadInput {
                    subject: Subject::parse("Everyone").unwrap(),
                    to: Addressees::Project,
                    item: Some(item),
                    label: SessionLabel::parse("web").unwrap(),
                    body: Body::parse("hello widgets").unwrap(),
                    importance: dibs_domain::message::Importance::Normal,
                    ack_required: false,
                },
            )
            .await
            .unwrap()
            .thread
            .record
            .id;

        let not_found = |result: Result<(), AppError>| {
            assert_eq!(result.unwrap_err().code(), "not_found");
        };
        not_found(app.get_item.execute(&dave, item).await.map(|_| ()));
        not_found(app.read_thread.execute(&dave, thread).await.map(|_| ()));
        not_found(app.mark_read.execute(&dave, thread).await.map(|_| ()));
        not_found(app.release.execute(&dave, item).await.map(|_| ()));
        not_found(
            app.claim_item
                .execute(
                    &dave,
                    ClaimItemInput {
                        wide: None,
                        item,
                        session: daves_session,
                        areas: Vec::new(),
                    },
                )
                .await
                .map(|_| ()),
        );
        not_found(
            app.heartbeat
                .execute(&dave, alices_session)
                .await
                .map(|_| ()),
        );
        assert!(
            app.list_items
                .execute(&dave, ItemQuery::default())
                .await
                .unwrap()
                .is_empty()
        );
        assert!(app.status.execute(&dave).await.unwrap().holds.is_empty());
        assert!(
            app.inbox
                .execute(&dave, unread_only(false))
                .await
                .unwrap()
                .threads
                .is_empty()
        );
        let daves_log = app
            .list_events
            .execute(&dave, EventCursor::Start, 500)
            .await
            .unwrap();
        assert!(daves_log.events.is_empty());
        let handles: Vec<String> = app
            .list_members
            .execute(&dave)
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.handle.to_string())
            .collect();
        assert_eq!(handles, ["dave"]);

        let alices_log = app
            .list_events
            .execute(&alice, EventCursor::Start, 500)
            .await
            .unwrap();
        assert!(!alices_log.events.is_empty());
        assert!(
            alices_log
                .events
                .iter()
                .all(|e| e.event.project == alice.project)
        );
    }

    #[tokio::test]
    async fn rotating_a_token_signs_its_browser_sessions_out() {
        let world = World::new().await;
        let app = &world.app;
        let alice = project(app, "widgets", "alice").await;
        let bob_token = app
            .create_member
            .execute(
                &alice,
                CreateMemberInput {
                    member: member("bob"),
                    role: Role::Member,
                },
            )
            .await
            .unwrap()
            .token;
        let browser = app
            .open_ui_session
            .execute(bob_token.expose())
            .await
            .unwrap();
        let signed_in = app
            .resolve_ui_session
            .execute(browser.secret.expose())
            .await
            .unwrap();
        assert_eq!(signed_in.role, Role::Member);

        app.rotate_token
            .execute(&alice, &Handle::parse("bob").unwrap())
            .await
            .unwrap();
        assert!(matches!(
            app.resolve_ui_session
                .execute(browser.secret.expose())
                .await
                .unwrap_err(),
            AppError::Unauthorized
        ));
        assert!(matches!(
            app.authenticate
                .execute(bob_token.expose())
                .await
                .unwrap_err(),
            AppError::Unauthorized
        ));
    }
    #[tokio::test]
    async fn an_import_repeated_on_the_file_writes_and_appends_nothing_and_feeds_the_ready_list() {
        use dibs_domain::import::{ImportBatch, Listing, ReportedItem};
        use dibs_domain::item::{Priority, SourceId};

        let world = World::new().await;
        let admin = project(&world.app, "widgets", "alice").await;
        let importer = world
            .app
            .create_member
            .execute(
                &admin,
                CreateMemberInput {
                    member: member("board"),
                    role: Role::Importer,
                },
            )
            .await
            .unwrap();
        let bot = world
            .app
            .authenticate
            .execute(importer.token.expose())
            .await
            .unwrap();
        let reported = |text: &str, blocked_by: &[&str]| ReportedItem {
            reference: ExternalRef::parse(text).unwrap(),
            title: Title::parse(&format!("Work on {text}")).unwrap(),
            closed: false,
            listing: Some(Listing {
                areas_hint: Vec::new(),
                priority: Priority::default(),
                source_status: None,
                blocked_by: blocked_by
                    .iter()
                    .map(|b| ExternalRef::parse(b).unwrap())
                    .collect(),
            }),
        };
        let batch = || {
            ImportBatch::new(
                Some(SourceId::parse("board").unwrap()),
                true,
                vec![
                    reported("gh:acme/web#1", &["gh:acme/web#2"]),
                    reported("gh:acme/web#2", &[]),
                ],
            )
            .unwrap()
        };
        world.app.upsert_items.execute(&bot, batch()).await.unwrap();
        let after_first = world.event_names(admin.project).await;
        assert_eq!(
            after_first[after_first.len() - 3..],
            ["item_created", "item_created", "dependency_added"]
        );
        world.app.upsert_items.execute(&bot, batch()).await.unwrap();
        assert_eq!(world.event_names(admin.project).await, after_first);
        let ready = world.app.ready.execute(&admin).await.unwrap();
        let refs: Vec<String> = ready
            .ready
            .iter()
            .map(|entry| entry.item.item.external_ref.as_ref().unwrap().to_string())
            .collect();
        assert_eq!(refs, ["gh:acme/web#2"]);
        assert_eq!(ready.excluded.len(), 1);
    }

    #[tokio::test]
    async fn an_ack_on_the_file_is_recorded_once_and_its_author_sees_it() {
        let world = World::new().await;
        let app = &world.app;
        let alice = project(app, "widgets", "alice").await;
        let token = app
            .create_member
            .execute(
                &alice,
                CreateMemberInput {
                    member: member("bob"),
                    role: Role::Member,
                },
            )
            .await
            .unwrap()
            .token;
        let bob = app.authenticate.execute(token.expose()).await.unwrap();
        world.fakes.clock.advance(Duration::minutes(1));
        let started = app
            .start_thread
            .execute(
                &alice,
                StartThreadInput {
                    subject: Subject::parse("Freeze main").unwrap(),
                    to: Addressees::Members(vec![Handle::parse("bob").unwrap()]),
                    item: None,
                    label: SessionLabel::parse("web").unwrap(),
                    body: Body::parse("Hold every push until noon.").unwrap(),
                    importance: dibs_domain::message::Importance::Urgent,
                    ack_required: true,
                },
            )
            .await
            .unwrap();
        let id = started.message.record.id;
        let waiting = app.unacked.execute(&bob).await.unwrap();
        assert_eq!(waiting.len(), 1);
        assert_eq!(
            waiting[0].message.record.importance,
            dibs_domain::message::Importance::Urgent
        );
        world.fakes.clock.advance(Duration::minutes(2));
        let first = app.ack.execute(&bob, id).await.unwrap();
        assert!(first.created);
        world.fakes.clock.advance(Duration::minutes(5));
        let again = app.ack.execute(&bob, id).await.unwrap();
        assert!(!again.created);
        assert_eq!(again.ack, first.ack, "the first acknowledgement stands");
        assert!(app.unacked.execute(&bob).await.unwrap().is_empty());
        let status = app.sent_status.execute(&alice, id).await.unwrap();
        assert_eq!(status.recipients.len(), 1);
        assert_eq!(
            status.recipients[0].acked_at,
            Some(OffsetDateTime::UNIX_EPOCH + Duration::minutes(3))
        );
        assert_eq!(
            world.event_names(alice.project).await.last(),
            Some(&"acked")
        );
    }

    /// One member's sessions all claim the delivery of one urgent message at once, over `store`:
    /// exactly one delivers it, and every other one is told it was delivered.
    async fn one_delivery_wins_a_race_of_sessions(store: Arc<dyn Store>) {
        let fakes = Fakes::new(OffsetDateTime::UNIX_EPOCH);
        let deps = Dependencies {
            store,
            ..fakes.dependencies()
        };
        let app = Arc::new(UseCases::new(&deps));
        let admin = project(&app, "widgets", "alice").await;
        let token = app
            .create_member
            .execute(
                &admin,
                CreateMemberInput {
                    member: member("bob"),
                    role: Role::Member,
                },
            )
            .await
            .unwrap()
            .token;
        let bob = app.authenticate.execute(token.expose()).await.unwrap();
        let mut sessions = Vec::new();
        for n in 0..RACERS {
            sessions.push(register(&app, &bob, n).await);
        }
        let sent = app
            .start_thread
            .execute(
                &admin,
                StartThreadInput {
                    subject: Subject::parse("Staging is down").unwrap(),
                    to: Addressees::Members(vec![Handle::parse("bob").unwrap()]),
                    item: None,
                    label: SessionLabel::parse("web").unwrap(),
                    body: Body::parse("Hold every deploy.").unwrap(),
                    importance: dibs_domain::message::Importance::Urgent,
                    ack_required: false,
                },
            )
            .await
            .unwrap()
            .message
            .record
            .id;
        let mut tasks = Vec::new();
        for session in sessions {
            let app = app.clone();
            let bob = bob.clone();
            tasks.push(tokio::spawn(async move {
                app.claim_delivery
                    .execute(&bob, session, DeliveryAsk::Message(sent))
                    .await
            }));
        }
        let mut won = 0;
        for task in tasks {
            match task.await.unwrap() {
                Ok(_) => won += 1,
                Err(error) => assert_eq!(error.code(), "delivered", "{error:?}"),
            }
        }
        assert_eq!(won, 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn one_delivery_wins_a_race_of_sessions_on_the_sqlite_store() {
        let world = World::new().await;
        one_delivery_wins_a_race_of_sessions(world.store.clone()).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn one_delivery_wins_a_race_of_sessions_on_the_memory_store() {
        one_delivery_wins_a_race_of_sessions(Arc::new(dibs_app::fakes::MemoryStore::default()))
            .await;
    }

    #[tokio::test]
    async fn retention_on_the_file_removes_by_date_and_keeps_an_open_items_thread() {
        let world = World::new().await;
        let app = &world.app;
        let alice = project(app, "widgets", "alice").await;
        let session = register(app, &alice, 1).await;
        let open = create_item(app, &alice, 1).await;
        app.claim_item
            .execute(
                &alice,
                ClaimItemInput {
                    item: open,
                    session,
                    areas: Vec::new(),
                    wide: None,
                },
            )
            .await
            .unwrap();
        let start = |subject: &str, item: Option<ItemId>| StartThreadInput {
            subject: Subject::parse(subject).unwrap(),
            to: Addressees::Project,
            item,
            label: SessionLabel::parse("web").unwrap(),
            body: Body::parse("notes").unwrap(),
            importance: dibs_domain::message::Importance::Normal,
            ack_required: false,
        };
        app.start_thread
            .execute(&alice, start("Old notes", None))
            .await
            .unwrap();
        app.start_thread
            .execute(&alice, start("About the open item", Some(open)))
            .await
            .unwrap();
        world.fakes.clock.advance(Duration::days(200));

        let report = app.prune.execute().await.unwrap();
        assert!(report.failed.is_empty());
        let (_, counts) = report.pruned[0];
        assert_eq!((counts.threads, counts.messages), (1, 1));
        let left: Vec<String> = app
            .list_threads
            .execute(&alice, None)
            .await
            .unwrap()
            .iter()
            .map(|summary| summary.thread.record.subject.to_string())
            .collect();
        assert_eq!(left, ["About the open item"]);
        assert_eq!(world.event_names(alice.project).await, ["pruned"]);
        let behind = app
            .list_events
            .execute(&alice, EventCursor::After(dibs_domain::ids::EventId(1)), 10)
            .await
            .unwrap_err();
        assert_eq!(behind.code(), "cursor_expired");
    }

    #[tokio::test]
    async fn a_removal_on_the_file_and_the_members_own_claim_meet_one_after_the_other() {
        let world = World::new().await;
        let app = &world.app;
        let admin = project(app, "widgets", "admin").await;
        let token = app
            .create_member
            .execute(
                &admin,
                CreateMemberInput {
                    member: member("bob"),
                    role: Role::Member,
                },
            )
            .await
            .unwrap()
            .token;
        let bob = app.authenticate.execute(token.expose()).await.unwrap();
        let session = register(app, &bob, 2).await;
        let items: Vec<ItemId> = {
            let mut items = Vec::new();
            for n in 0..8 {
                items.push(create_item(app, &admin, 10 + n).await);
            }
            items
        };
        let world = Arc::new(world);
        let mut tasks = Vec::new();
        for item in items.iter().copied() {
            let (world, bob) = (world.clone(), bob.clone());
            tasks.push(tokio::spawn(async move {
                world
                    .app
                    .claim_item
                    .execute(
                        &bob,
                        ClaimItemInput {
                            item,
                            session,
                            areas: Vec::new(),
                            wide: None,
                        },
                    )
                    .await
                    .is_ok()
            }));
        }
        let remover = {
            let (world, admin) = (world.clone(), admin.clone());
            tokio::spawn(async move {
                world
                    .app
                    .remove_member
                    .execute(&admin, &Handle::parse("bob").unwrap())
                    .await
                    .unwrap()
            })
        };
        let mut claimed = 0;
        for task in tasks {
            if task.await.unwrap() {
                claimed += 1;
            }
        }
        let removed = remover.await.unwrap();
        assert_eq!(
            removed.released.len(),
            claimed,
            "every claim made before the removal was released by it, and none was made after"
        );
        let mut tx = world.store.begin().await.unwrap();
        let held: Vec<_> = tx
            .claims(admin.project)
            .await
            .unwrap()
            .into_iter()
            .filter(|claim| claim.member == bob.member)
            .collect();
        assert!(
            held.is_empty(),
            "nothing is left in a removed member's name"
        );
    }

    #[tokio::test]
    async fn messages_racing_a_removal_on_the_file_land_before_it_or_not_at_all() {
        use dibs_app::use_cases::PostMessageInput;
        use dibs_domain::event::EventKind;

        let world = World::new().await;
        let app = &world.app;
        let admin = project(app, "widgets", "admin").await;
        let token = app
            .create_member
            .execute(
                &admin,
                CreateMemberInput {
                    member: member("bob"),
                    role: Role::Member,
                },
            )
            .await
            .unwrap()
            .token;
        let bob = app.authenticate.execute(token.expose()).await.unwrap();
        let thread = app
            .start_thread
            .execute(
                &admin,
                StartThreadInput {
                    subject: Subject::parse("Rates").unwrap(),
                    to: Addressees::Project,
                    item: None,
                    label: SessionLabel::parse("web").unwrap(),
                    body: Body::parse("Where is the table?").unwrap(),
                    importance: dibs_domain::message::Importance::Normal,
                    ack_required: false,
                },
            )
            .await
            .unwrap()
            .thread
            .record
            .id;
        let world = Arc::new(world);
        let mut tasks = Vec::new();
        for n in 0..RACERS {
            let (world, bob) = (world.clone(), bob.clone());
            tasks.push(tokio::spawn(async move {
                world
                    .app
                    .post_message
                    .execute(
                        &bob,
                        PostMessageInput {
                            thread,
                            label: SessionLabel::parse("web").unwrap(),
                            body: Body::parse(&format!("note {n}")).unwrap(),
                            importance: dibs_domain::message::Importance::Normal,
                            ack_required: false,
                        },
                    )
                    .await
            }));
        }
        let remover = {
            let (world, admin) = (world.clone(), admin.clone());
            tokio::spawn(async move {
                world
                    .app
                    .remove_member
                    .execute(&admin, &Handle::parse("bob").unwrap())
                    .await
                    .unwrap()
            })
        };
        let mut posted = 0;
        for task in tasks {
            match task.await.unwrap() {
                Ok(_) => posted += 1,
                Err(error) => assert_eq!(error.code(), "unauthorized"),
            }
        }
        remover.await.unwrap();
        let mut tx = world.store.begin().await.unwrap();
        let events = tx.events_after(admin.project, None, 10_000).await.unwrap();
        let removed_at = events
            .iter()
            .position(|event| matches!(event.kind, EventKind::MemberRemoved(_)))
            .unwrap();
        let by_bob: Vec<usize> = events
            .iter()
            .enumerate()
            .filter(|(_, event)| {
                matches!(&event.kind, EventKind::MessagePosted(posted) if posted.author == Some(bob.member))
            })
            .map(|(place, _)| place)
            .collect();
        assert_eq!(by_bob.len(), posted);
        assert!(
            by_bob.iter().all(|place| *place < removed_at),
            "no message lands after the removal"
        );
    }
}
