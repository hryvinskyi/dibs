//! Two projects in one service, through the public API only: a token of one never reaches the
//! other's records, and tokens and the browser sessions they open end together.

#[cfg(test)]
mod tenancy {
    use time::OffsetDateTime;

    use dibs_app::fakes::Fakes;
    use dibs_app::use_cases::{
        Addressees, CreateItemInput, CreateMemberInput, EventCursor, InitProjectInput, ItemQuery,
        NewMember, RegisterSessionInput, StartThreadInput, UseCases,
    };
    use dibs_app::{Actor, AppError};
    use dibs_domain::handle::Handle;
    use dibs_domain::ids::SessionId;
    use dibs_domain::item::Title;
    use dibs_domain::member::DisplayName;
    use dibs_domain::member::Role;
    use dibs_domain::message::{Body, SessionLabel, Subject};
    use dibs_domain::project::ProjectSlug;
    use dibs_domain::session::AgentLabel;
    use dibs_domain::session::HostName;

    fn unread_only(unread_only: bool) -> dibs_app::use_cases::InboxQuery {
        dibs_app::use_cases::InboxQuery {
            unread_only,
            unacked: false,
        }
    }

    fn member(handle: &str) -> NewMember {
        NewMember {
            handle: Handle::parse(handle).unwrap(),
            display_name: DisplayName::parse(handle).unwrap(),
            external_login: None,
        }
    }

    async fn project(app: &UseCases, slug: &str, admin: &str) -> (Actor, SessionId) {
        let created = app
            .init_project
            .execute(InitProjectInput {
                slug: ProjectSlug::parse(slug).unwrap(),
                admin: member(admin),
            })
            .await
            .unwrap();
        let actor = app
            .authenticate
            .execute(created.token.expose())
            .await
            .unwrap();
        let session = app
            .register_session
            .execute(
                &actor,
                RegisterSessionInput {
                    host: HostName::parse("laptop").unwrap(),
                    label: SessionLabel::parse("laptop/mcp").unwrap(),
                    agent: AgentLabel::parse("agent").unwrap(),
                    push: false,
                },
            )
            .await
            .unwrap()
            .id;
        (actor, session)
    }

    #[tokio::test]
    async fn a_token_of_one_project_never_reaches_another() {
        let fakes = Fakes::new(OffsetDateTime::UNIX_EPOCH);
        let app = UseCases::new(&fakes.dependencies());
        let (alice, _) = project(&app, "widgets", "alice").await;
        let (dave, daves_session) = project(&app, "gadgets", "dave").await;
        assert_ne!(alice.project, dave.project);

        let item = app
            .create_item
            .execute(
                &alice,
                CreateItemInput {
                    external_ref: None,
                    title: Title::parse("Private to widgets").unwrap(),
                },
            )
            .await
            .unwrap()
            .item
            .item
            .id;
        let thread = app
            .start_thread
            .execute(
                &alice,
                StartThreadInput {
                    subject: Subject::parse("Everyone").unwrap(),
                    to: Addressees::Project,
                    item: None,
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
            app.heartbeat
                .execute(&alice, daves_session)
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
    }

    #[tokio::test]
    async fn rotating_a_token_signs_its_browser_sessions_out() {
        let fakes = Fakes::new(OffsetDateTime::UNIX_EPOCH);
        let app = UseCases::new(&fakes.dependencies());
        let (alice, _) = project(&app, "widgets", "alice").await;
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

        let rotated = app
            .rotate_token
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
        let again = app
            .open_ui_session
            .execute(rotated.token.expose())
            .await
            .unwrap();
        app.close_ui_session
            .execute(again.secret.expose())
            .await
            .unwrap();
        assert!(matches!(
            app.resolve_ui_session
                .execute(again.secret.expose())
                .await
                .unwrap_err(),
            AppError::Unauthorized
        ));
    }
}
