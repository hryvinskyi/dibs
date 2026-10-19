//! A project's working day through the public API only: a project set up, two people claiming,
//! checking edits, granting, writing to each other, a stale claim taken over, work completed — and
//! the event log and the wake-ups that record it.

#[cfg(test)]
mod scenario {
    use std::sync::Arc;

    use time::{Duration, OffsetDateTime};

    use dibs_app::fakes::Fakes;
    use dibs_app::named::Refusal;
    use dibs_app::use_cases::{
        Addressees, ClaimItemInput, CreateItemInput, CreateMemberInput, EditCheckInput,
        EventCursor, GrantInput, InitProjectInput, ItemQuery, NewMember, PostMessageInput,
        RegisterSessionInput, StartThreadInput, TakeOverInput, UseCases,
    };
    use dibs_app::{Actor, AppError};
    use dibs_domain::claim::ClaimOutcome;
    use dibs_domain::edit::Verdict;
    use dibs_domain::event::EventKind;
    use dibs_domain::handle::Handle;
    use dibs_domain::ids::{ItemId, SessionId};
    use dibs_domain::item::{ExternalRef, ItemStatus, Title};
    use dibs_domain::member::DisplayName;
    use dibs_domain::member::Role;
    use dibs_domain::message::{Body, SessionLabel, Subject};
    use dibs_domain::paths::RepoPath;
    use dibs_domain::project::ProjectSlug;
    use dibs_domain::session::AgentLabel;
    use dibs_domain::session::HostName;
    use dibs_domain::settings::SettingsDocument;

    fn unread_only(unread_only: bool) -> dibs_app::use_cases::InboxQuery {
        dibs_app::use_cases::InboxQuery {
            unread_only,
            unacked: false,
        }
    }

    const SETTINGS: &str = r#"{
        "stale_after": "24h",
        "edit_policy": "require-claim",
        "areas": [
            {"name": "billing", "paths": ["packages/billing/**"]},
            {"name": "search", "paths": ["packages/search/**"]}
        ],
        "hot": [
            {"paths": ["**/*.lock"], "kind": "regenerate", "note": "regenerate it; never merge by hand"},
            {"paths": ["config/**"], "kind": "warn-shared"}
        ]
    }"#;

    fn member(handle: &str) -> NewMember {
        NewMember {
            handle: Handle::parse(handle).unwrap(),
            display_name: DisplayName::parse(handle).unwrap(),
            external_login: None,
        }
    }

    fn path(text: &str) -> RepoPath {
        RepoPath::parse(text).unwrap()
    }

    struct Team {
        fakes: Fakes,
        app: UseCases,
        alice: Actor,
        bob: Actor,
        alice_session: SessionId,
        bob_session: SessionId,
    }

    impl Team {
        async fn new() -> Self {
            let fakes = Fakes::new(OffsetDateTime::UNIX_EPOCH);
            let app = UseCases::new(&fakes.dependencies());
            let created = app
                .init_project
                .execute(InitProjectInput {
                    slug: ProjectSlug::parse("widgets").unwrap(),
                    admin: member("alice"),
                })
                .await
                .unwrap();
            let alice = app
                .authenticate
                .execute(created.token.expose())
                .await
                .unwrap();
            let document: SettingsDocument = serde_json::from_str(SETTINGS).unwrap();
            app.apply_settings.execute(&alice, document).await.unwrap();
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
            let bob = app.authenticate.execute(bob_token.expose()).await.unwrap();
            let register = |host: &str, label: &str| RegisterSessionInput {
                host: HostName::parse(host).unwrap(),
                label: SessionLabel::parse(label).unwrap(),
                agent: AgentLabel::parse("agent").unwrap(),
                push: true,
            };
            let alice_session = app
                .register_session
                .execute(&alice, register("laptop", "laptop/4f2a91c0"))
                .await
                .unwrap()
                .id;
            let bob_session = app
                .register_session
                .execute(&bob, register("desktop", "desktop/77aa01bc"))
                .await
                .unwrap()
                .id;
            Self {
                fakes,
                app,
                alice,
                bob,
                alice_session,
                bob_session,
            }
        }

        async fn item(&self, reference: &str, title: &str) -> ItemId {
            self.app
                .create_item
                .execute(
                    &self.alice,
                    CreateItemInput {
                        external_ref: Some(ExternalRef::parse(reference).unwrap()),
                        title: Title::parse(title).unwrap(),
                    },
                )
                .await
                .unwrap()
                .item
                .item
                .id
        }

        async fn verdicts(&self, who: &Actor, session: SessionId, paths: &[&str]) -> Vec<Verdict> {
            self.app
                .edit_check
                .execute(
                    who,
                    EditCheckInput {
                        session,
                        paths: paths.iter().map(|p| path(p)).collect(),
                        reserve: true,
                    },
                )
                .await
                .unwrap()
                .verdicts
                .into_iter()
                .map(|v| v.verdict.verdict)
                .collect()
        }

        async fn event_names(&self) -> Vec<&'static str> {
            self.app
                .list_events
                .execute(&self.bob, EventCursor::Start, 500)
                .await
                .unwrap()
                .events
                .iter()
                .map(|e| e.event.kind.name())
                .collect()
        }
    }

    #[tokio::test]
    async fn a_working_day() {
        let team = Team::new().await;
        let app = &team.app;
        let invoices = team
            .item("gh:acme/widgets#42", "Fix the invoice total")
            .await;
        let ranking = team.item("gh:acme/widgets#43", "Tune search ranking").await;

        // Bob resolves the reference he was given to the same item.
        let found = app
            .list_items
            .execute(
                &team.bob,
                ItemQuery {
                    external_ref: Some(ExternalRef::parse("gh:acme/widgets#42").unwrap()),
                    ..ItemQuery::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            found.iter().map(|i| i.item.id).collect::<Vec<_>>(),
            [invoices]
        );

        // Without a claim, Bob may not touch billing.
        assert_eq!(
            team.verdicts(
                &team.bob,
                team.bob_session,
                &["default:packages/billing/total.rs"]
            )
            .await,
            [Verdict::NeedsClaim]
        );

        // Alice takes billing; Bob takes search and cannot take billing as well.
        let claim = |item, session, areas: &[&str]| ClaimItemInput {
            item,
            session,
            areas: areas.iter().map(|a| (*a).to_owned()).collect(),
        };
        let outcome = app
            .claim_item
            .execute(
                &team.alice,
                claim(invoices, team.alice_session, &["billing"]),
            )
            .await
            .unwrap();
        assert!(matches!(outcome.outcome, ClaimOutcome::New { .. }));
        assert_eq!(outcome.holder.as_str(), "alice");
        let refused = app
            .claim_item
            .execute(
                &team.bob,
                claim(ranking, team.bob_session, &["search", "billing"]),
            )
            .await
            .unwrap_err();
        assert_eq!(refused.code(), "area_held");
        assert!(matches!(
            &refused,
            AppError::Claim(Refusal { member: Some(handle), .. }) if handle.as_str() == "alice"
        ));
        app.claim_item
            .execute(&team.bob, claim(ranking, team.bob_session, &["search"]))
            .await
            .unwrap();

        // Bob's edit in billing is held by Alice until she grants it for his item.
        let held = team
            .verdicts(
                &team.bob,
                team.bob_session,
                &["default:packages/billing/total.rs"],
            )
            .await;
        assert!(matches!(held.as_slice(), [Verdict::HeldBy { item, .. }] if *item == invoices));
        let grant = app
            .grant
            .execute(
                &team.alice,
                GrantInput {
                    item: ranking,
                    area: "billing".to_owned(),
                    to: Handle::parse("bob").unwrap(),
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            team.verdicts(
                &team.bob,
                team.bob_session,
                &["default:packages/billing/total.rs"]
            )
            .await
            .as_slice(),
            [Verdict::Granted { .. }]
        ));

        // Bob tells Alice; she reads and answers.
        let started = app
            .start_thread
            .execute(
                &team.bob,
                StartThreadInput {
                    subject: Subject::parse("Touching billing/total.rs").unwrap(),
                    to: Addressees::Members(vec![Handle::parse("alice").unwrap()]),
                    item: Some(ranking),
                    label: SessionLabel::parse("desktop/77aa01bc").unwrap(),
                    body: Body::parse("One line in the rounding helper.\nThanks!").unwrap(),
                    importance: dibs_domain::message::Importance::Normal,
                    ack_required: false,
                },
            )
            .await
            .unwrap();
        let inbox = app
            .inbox
            .execute(&team.alice, unread_only(true))
            .await
            .unwrap()
            .threads;
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].thread.record.id, started.thread.record.id);
        let thread = app
            .read_thread
            .execute(&team.alice, started.thread.record.id)
            .await
            .unwrap();
        assert_eq!(
            thread.messages[0].record.body.as_str(),
            "One line in the rounding helper.\nThanks!"
        );
        app.mark_read
            .execute(&team.alice, started.thread.record.id)
            .await
            .unwrap();
        app.post_message
            .execute(
                &team.alice,
                PostMessageInput {
                    thread: started.thread.record.id,
                    label: SessionLabel::parse("web").unwrap(),
                    body: Body::parse("Go ahead.").unwrap(),
                    importance: dibs_domain::message::Importance::Normal,
                    ack_required: false,
                },
            )
            .await
            .unwrap();
        assert!(
            app.inbox
                .execute(&team.alice, unread_only(true))
                .await
                .unwrap()
                .threads
                .is_empty()
        );
        assert_eq!(
            app.inbox
                .execute(&team.bob, unread_only(true))
                .await
                .unwrap()
                .threads
                .len(),
            1
        );

        // Alice goes quiet for a day; Bob's heartbeats keep his own claim live.
        team.fakes.clock.advance(Duration::hours(12));
        let beat = app
            .heartbeat
            .execute(&team.bob, team.bob_session)
            .await
            .unwrap();
        assert_eq!(beat.refreshed, [ranking]);
        team.fakes.clock.advance(Duration::hours(13));
        let status = app.status.execute(&team.bob).await.unwrap();
        assert_eq!(
            status
                .stale
                .iter()
                .map(|h| h.item.item.id)
                .collect::<Vec<_>>(),
            [invoices]
        );
        assert_eq!(
            status
                .live
                .iter()
                .map(|h| h.item.item.id)
                .collect::<Vec<_>>(),
            [ranking]
        );

        // Bob takes the invoice over; Alice's grant ends with her claim.
        let taken = app
            .take_over
            .execute(
                &team.bob,
                TakeOverInput {
                    item: invoices,
                    session: team.bob_session,
                    areas: Vec::new(),
                    reason: Body::parse("You have been away a day; finishing it.").unwrap(),
                },
            )
            .await
            .unwrap();
        assert_eq!(taken.takeover.previous.member, team.alice.member);
        let alices = app
            .inbox
            .execute(&team.alice, unread_only(true))
            .await
            .unwrap()
            .threads;
        assert_eq!(alices.len(), 1);
        assert_eq!(alices[0].thread.record.id, taken.thread);
        let after_takeover = app
            .revoke_grant
            .execute(&team.alice, grant.grant.record.id)
            .await
            .unwrap_err();
        assert_eq!(after_takeover.code(), "not_found");

        // Bob completes the invoice; nobody may claim it again.
        app.complete.execute(&team.bob, invoices).await.unwrap();
        let item = app.get_item.execute(&team.alice, invoices).await.unwrap();
        assert_eq!(item.item.item.status, ItemStatus::Done);
        assert_eq!(item.claim, None);
        let closed = app
            .claim_item
            .execute(&team.alice, claim(invoices, team.alice_session, &[]))
            .await
            .unwrap_err();
        assert!(matches!(closed, AppError::ItemClosed { .. }));

        // The log tells the whole story, in order, and the last wake-up names its last entry.
        assert_eq!(
            team.event_names().await,
            [
                "settings_changed",
                "item_created",
                "item_created",
                "claimed",
                "claimed",
                "granted",
                "thread_started",
                "message_posted",
                "message_posted",
                "taken_over",
                "thread_started",
                "message_posted",
                "grant_revoked",
                "completed",
            ]
        );
        let last = app
            .list_events
            .execute(&team.bob, EventCursor::Latest, 1)
            .await
            .unwrap()
            .next
            .unwrap();
        let calls = team.fakes.notifier.calls();
        assert_eq!(calls.last().map(|(_, id)| *id), Some(last));
        let events = team.fakes.store.all_events().await;
        assert!(events.windows(2).all(|pair| pair[0].id < pair[1].id));
        let log = serde_json::to_string(
            &events
                .iter()
                .map(|e| e.kind.clone())
                .collect::<Vec<EventKind>>(),
        )
        .unwrap();
        assert!(!log.contains("rounding helper"));
        assert!(!log.contains("Go ahead"));
    }

    #[tokio::test]
    async fn use_cases_built_once_are_shared_across_tasks() {
        let team = Arc::new(Team::new().await);
        let item = team.item("gh:acme/widgets#7", "Shared").await;
        let task = {
            let team = team.clone();
            tokio::spawn(async move { team.app.get_item.execute(&team.bob, item).await })
        };
        let view = task.await.unwrap().unwrap();
        assert_eq!(view.item.item.id, item);
    }
}
