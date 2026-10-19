//! One project over several repositories, through the public API only: settings that list
//! repositories, areas of the same name in two of them, a claim that spans both, edit checks on
//! qualified paths, and items whose references name a repository.

#[cfg(test)]
mod repositories {
    use time::OffsetDateTime;

    use dibs_app::Actor;
    use dibs_app::fakes::Fakes;
    use dibs_app::use_cases::{
        ClaimItemInput, CreateItemInput, CreateMemberInput, EditCheckInput, InitProjectInput,
        NewMember, RegisterSessionInput, UseCases,
    };
    use dibs_domain::area::QualifiedArea;
    use dibs_domain::claim::ClaimOutcome;
    use dibs_domain::edit::Verdict;
    use dibs_domain::event::EventKind;
    use dibs_domain::handle::Handle;
    use dibs_domain::ids::{ItemId, SessionId};
    use dibs_domain::item::{ExternalRef, ItemRepository, Title};
    use dibs_domain::member::DisplayName;
    use dibs_domain::member::Role;
    use dibs_domain::message::SessionLabel;
    use dibs_domain::paths::RepoPath;
    use dibs_domain::project::ProjectSlug;
    use dibs_domain::repo::RepoId;
    use dibs_domain::session::AgentLabel;
    use dibs_domain::session::HostName;
    use dibs_domain::settings::SettingsDocument;

    const SETTINGS: &str = r#"{
        "repos": [
            {
                "id": "api",
                "remote": "https://github.com/acme/api.git",
                "areas": [
                    {"name": "billing", "paths": ["src/billing/**"]},
                    {"name": "schema", "paths": ["schema/**"]}
                ],
                "hot": [{"paths": ["Cargo.lock"], "kind": "regenerate"}]
            },
            {
                "id": "web",
                "remote": "git@github.com:acme/web.git",
                "areas": [
                    {"name": "billing", "paths": ["app/billing/**"]},
                    {"name": "checkout", "paths": ["app/checkout/**"]}
                ]
            }
        ]
    }"#;

    fn new_member(handle: &str) -> NewMember {
        NewMember {
            handle: Handle::parse(handle).unwrap(),
            display_name: DisplayName::parse(handle).unwrap(),
            external_login: None,
        }
    }

    fn area(text: &str) -> QualifiedArea {
        QualifiedArea::parse(text).unwrap()
    }

    struct Project {
        app: UseCases,
        alice: Actor,
        bob: Actor,
        importer: Actor,
        alice_session: SessionId,
        bob_session: SessionId,
    }

    impl Project {
        async fn new() -> Self {
            let fakes = Fakes::new(OffsetDateTime::UNIX_EPOCH);
            let app = UseCases::new(&fakes.dependencies());
            let created = app
                .init_project
                .execute(InitProjectInput {
                    slug: ProjectSlug::parse("acme").unwrap(),
                    admin: new_member("alice"),
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
            let mut actors = Vec::new();
            for (handle, role) in [("bob", Role::Member), ("importer", Role::Importer)] {
                let token = app
                    .create_member
                    .execute(
                        &alice,
                        CreateMemberInput {
                            member: new_member(handle),
                            role,
                        },
                    )
                    .await
                    .unwrap()
                    .token;
                actors.push(app.authenticate.execute(token.expose()).await.unwrap());
            }
            let importer = actors.pop().unwrap();
            let bob = actors.pop().unwrap();
            let mut sessions = Vec::new();
            for (actor, host) in [(&alice, "laptop"), (&bob, "desktop")] {
                let session = app
                    .register_session
                    .execute(
                        actor,
                        RegisterSessionInput {
                            host: HostName::parse(host).unwrap(),
                            label: SessionLabel::parse(&format!("{host}/mcp")).unwrap(),
                            agent: AgentLabel::parse("agent").unwrap(),
                            push: false,
                        },
                    )
                    .await
                    .unwrap();
                sessions.push(session.id);
            }
            Self {
                app,
                alice,
                bob,
                importer,
                alice_session: sessions[0],
                bob_session: sessions[1],
            }
        }

        async fn create(&self, actor: &Actor, reference: &str) -> Result<ItemId, String> {
            self.app
                .create_item
                .execute(
                    actor,
                    CreateItemInput {
                        external_ref: Some(ExternalRef::parse(reference).unwrap()),
                        title: Title::parse("Work").unwrap(),
                    },
                )
                .await
                .map(|created| created.item.id)
                .map_err(|error| error.code().to_owned())
        }

        async fn claim(
            &self,
            actor: &Actor,
            session: SessionId,
            item: ItemId,
            areas: &[&str],
        ) -> Result<ClaimOutcome, String> {
            self.app
                .claim_item
                .execute(
                    actor,
                    ClaimItemInput {
                        item,
                        session,
                        areas: areas.iter().map(|a| (*a).to_owned()).collect(),
                    },
                )
                .await
                .map_err(|error| error.code().to_owned())
        }

        async fn verdicts(
            &self,
            actor: &Actor,
            session: SessionId,
            paths: &[&str],
        ) -> Vec<Verdict> {
            self.app
                .edit_check
                .execute(
                    actor,
                    EditCheckInput {
                        session,
                        paths: paths.iter().map(|p| RepoPath::parse(p).unwrap()).collect(),
                        reserve: true,
                    },
                )
                .await
                .unwrap()
                .verdicts
                .into_iter()
                .map(|v| v.verdict)
                .collect()
        }
    }

    #[tokio::test]
    async fn a_claim_spans_two_repositories_and_their_areas_stay_apart() {
        let project = Project::new().await;
        let api_change = project
            .create(&project.alice, "gh:acme/api#12")
            .await
            .unwrap();
        let web_change = project
            .create(&project.bob, "gh:acme/web#30")
            .await
            .unwrap();

        // `billing` exists in both repositories, so the bare name is ambiguous.
        assert_eq!(
            project
                .claim(
                    &project.alice,
                    project.alice_session,
                    api_change,
                    &["billing"]
                )
                .await
                .unwrap_err(),
            "ambiguous_area"
        );

        // Alice's change covers the API's billing and its schema together.
        let outcome = project
            .claim(
                &project.alice,
                project.alice_session,
                api_change,
                &["api:billing", "schema"],
            )
            .await
            .unwrap();
        assert_eq!(
            outcome.claim().areas,
            [area("api:billing"), area("api:schema")].into()
        );

        // The web's billing is another area, free for Bob.
        let outcome = project
            .claim(
                &project.bob,
                project.bob_session,
                web_change,
                &["web:billing", "checkout"],
            )
            .await
            .unwrap();
        assert_eq!(
            outcome.claim().areas,
            [area("web:billing"), area("web:checkout")].into()
        );
        assert_eq!(
            project
                .claim(
                    &project.bob,
                    project.bob_session,
                    web_change,
                    &["api:schema"]
                )
                .await
                .unwrap_err(),
            "area_held"
        );

        // Edit checks answer per repository.
        let alice_sees = project
            .verdicts(
                &project.alice,
                project.alice_session,
                &[
                    "api:src/billing/invoice.rs",
                    "web:app/billing/invoice.ts",
                    "api:Cargo.lock",
                    "web:README.md",
                ],
            )
            .await;
        assert_eq!(alice_sees[0], Verdict::Allowed);
        assert!(matches!(
            &alice_sees[1],
            Verdict::HeldBy { area: held, .. } if *held == area("web:billing")
        ));
        assert!(matches!(alice_sees[2], Verdict::AllowedWithNote { .. }));
        assert_eq!(alice_sees[3], Verdict::Allowed);

        // A path in a repository the project does not list refuses the check.
        let unknown = project
            .app
            .edit_check
            .execute(
                &project.alice,
                EditCheckInput {
                    session: project.alice_session,
                    paths: vec![RepoPath::parse("search:src/lib.rs").unwrap()],
                    reserve: false,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(unknown.code(), "unknown_repo");

        // Qualified names travel in full through the log.
        let events = project
            .app
            .list_events
            .execute(
                &project.importer,
                dibs_app::use_cases::EventCursor::Start,
                100,
            )
            .await
            .unwrap()
            .events;
        let claimed: Vec<&EventKind> = events
            .iter()
            .map(|e| &e.kind)
            .filter(|k| matches!(k, EventKind::Claimed(_)))
            .collect();
        let json = serde_json::to_string(&claimed).unwrap();
        assert!(json.contains("\"api:billing\""), "{json}");
        assert!(json.contains("\"web:checkout\""), "{json}");
    }

    #[tokio::test]
    async fn items_name_their_repository_and_unlisted_ones_are_the_importers_to_create() {
        let project = Project::new().await;
        let listed = project.create(&project.bob, "gh:acme/web#5").await.unwrap();
        let view = project
            .app
            .get_item
            .execute(&project.bob, listed)
            .await
            .unwrap();
        assert_eq!(
            view.item.repository,
            ItemRepository::Listed(RepoId::parse("web").unwrap())
        );

        assert_eq!(
            project
                .create(&project.bob, "gh:acme/search#9")
                .await
                .unwrap_err(),
            "unlisted_repository"
        );
        let blocker = project
            .create(&project.importer, "gh:acme/search#9")
            .await
            .unwrap();

        // Such an item may be claimed, without areas: none exist outside listed repositories.
        let outcome = project
            .claim(&project.bob, project.bob_session, blocker, &[])
            .await
            .unwrap();
        assert!(outcome.claim().areas.is_empty());
        assert_eq!(
            project
                .claim(&project.bob, project.bob_session, blocker, &["search:core"])
                .await
                .unwrap_err(),
            "unknown_area"
        );
    }

    #[tokio::test]
    async fn an_area_added_by_an_edit_is_qualified_by_the_paths_repository() {
        let project = Project::new().await;
        let item = project
            .create(&project.bob, "gh:acme/web#40")
            .await
            .unwrap();
        project
            .claim(&project.bob, project.bob_session, item, &[])
            .await
            .unwrap();
        project
            .verdicts(
                &project.bob,
                project.bob_session,
                &["web:app/billing/cart.ts"],
            )
            .await;
        let view = project
            .app
            .get_item
            .execute(&project.bob, item)
            .await
            .unwrap();
        assert_eq!(view.claim.unwrap().areas, [area("web:billing")].into());
        let alices = project
            .verdicts(
                &project.alice,
                project.alice_session,
                &["api:src/billing/a.rs"],
            )
            .await;
        assert_eq!(alices, [Verdict::NeedsClaim]);
    }
}
