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
        Addressees, ClaimItemInput, ClaimMade, CreateItemInput, CreateMemberInput, EventCursor,
        InitProjectInput, ItemQuery, NewMember, RegisterSessionInput, StartThreadInput,
        TakeOverInput, UseCases,
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
    use dibs_store_sqlite::SqliteStore;

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
            let document: SettingsDocument =
                serde_json::from_str(r#"{"areas":[{"name":"billing","paths":["billing/**"]}]}"#)
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
        assert!(app.inbox.execute(&dave, false).await.unwrap().is_empty());
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
}
