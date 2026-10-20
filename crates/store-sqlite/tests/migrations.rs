//! Each migration on a database the one before it created, with rows in it: the rows are still read
//! through the store afterwards, and what a migration refuses to carry over fails it, saying what.

#[cfg(test)]
mod migrations {
    use std::path::{Path, PathBuf};

    use sqlx::migrate::Migrator;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use tempfile::TempDir;

    use dibs_app::ports::Store;
    use dibs_app::records::ItemFilter;
    use dibs_domain::area::QualifiedArea;
    use dibs_domain::claim::Claim;
    use dibs_domain::ids::{ItemId, MemberId, MessageId, ProjectId, ThreadId};
    use dibs_domain::item::{ExternalRef, ItemStatus, Priority};
    use dibs_domain::message::{Ack, Importance};
    use dibs_domain::wide::{WideAsk, WideReason, WideScope};
    use dibs_store_sqlite::{OpenError, SqliteStore};

    static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

    /// A database file brought up to `version` only, with `rows` run on it.
    async fn database_at(version: i64, rows: &str) -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dibs.db");
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        MIGRATOR.run_to(version, &pool).await.unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(rows.to_owned()))
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
        (dir, path)
    }

    /// A project, its member and items under the first schema.
    fn items_at_first_schema(references: &[&str]) -> String {
        let mut sql = String::from(
            "INSERT INTO projects (id, slug, created_at) VALUES (1, 'widgets', 0);\n\
             INSERT INTO members (project_id, id, handle, display_name, external_login, role, \
             created_at) VALUES (1, 11, 'alice', 'Alice', NULL, 'admin', 0);\n",
        );
        for (n, reference) in references.iter().enumerate() {
            sql.push_str(&format!(
                "INSERT INTO items (project_id, id, external_ref, title, status, repo, remote, \
                 created_by, created_at) VALUES (1, {}, '{reference}', 'Work', 'open', NULL, NULL, \
                 11, {n});\n",
                51 + n
            ));
        }
        sql
    }

    fn refusal(error: &OpenError) -> String {
        let OpenError::Migrate(cause) = error else {
            panic!("not a migration failure: {error:?}");
        };
        format!("{cause} {cause:?}")
    }

    async fn open(path: &Path) -> Result<SqliteStore, OpenError> {
        SqliteStore::open(path).await
    }

    #[tokio::test]
    async fn ready_carries_the_items_over_with_defaults_and_one_form_of_gh_references() {
        let (_dir, path) = database_at(
            1,
            &items_at_first_schema(&["gh:Acme/Widgets#1", "jira:ACME-7", "gh:acme/api#2"]),
        )
        .await;
        let store = open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        let items = tx
            .items(ProjectId(1), &ItemFilter::default())
            .await
            .unwrap();
        let references: Vec<String> = items
            .iter()
            .map(|item| item.external_ref.as_ref().unwrap().to_string())
            .collect();
        assert_eq!(
            references,
            ["gh:acme/widgets#1", "jira:ACME-7", "gh:acme/api#2"]
        );
        for item in &items {
            assert_eq!(item.priority, Priority::default());
            assert!(item.areas_hint.is_empty());
            assert_eq!((item.source_status.clone(), item.closed), (None, false));
            assert_eq!(item.source, None);
            assert_eq!(item.status, ItemStatus::Open);
        }
        let typed = ExternalRef::parse("gh:ACME/widgets#1").unwrap();
        assert_eq!(
            tx.item_by_ref(ProjectId(1), &typed)
                .await
                .unwrap()
                .map(|i| i.id),
            Some(ItemId(51)),
            "a person typing another case finds the stored item"
        );
        assert!(tx.dependencies(ProjectId(1)).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn ready_refuses_two_spellings_of_one_ticket_naming_both() {
        let (_dir, path) = database_at(
            1,
            &items_at_first_schema(&["gh:Acme/Web#3", "gh:acme/web#3"]),
        )
        .await;
        let text = refusal(&open(&path).await.unwrap_err());
        assert!(text.contains("gh:Acme/Web#3"), "{text}");
        assert!(text.contains("gh:acme/web#3"), "{text}");
        assert!(text.contains("merge them by hand"), "{text}");

        let (_dir, path) = database_at(
            1,
            &items_at_first_schema(&["gh:Acme/Web#3", "gh:acme/web#3"]),
        )
        .await;
        let pool = SqlitePoolOptions::new()
            .connect_with(SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
        assert!(MIGRATOR.run(&pool).await.is_err());
        let (version,): (i64,) = sqlx::query_as("SELECT max(version) FROM _sqlx_migrations")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(version, 1, "nothing of the refused migration is kept");
        let (stored,): (String,) =
            sqlx::query_as("SELECT group_concat(external_ref, ' ') FROM items ORDER BY id")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stored, "gh:Acme/Web#3 gh:acme/web#3");
    }

    #[tokio::test]
    async fn ready_refuses_a_gh_reference_without_the_gh_shape_naming_it() {
        let (_dir, path) = database_at(
            1,
            &items_at_first_schema(&["gh:acme/web#1", "gh:not-an-issue"]),
        )
        .await;
        let text = refusal(&open(&path).await.unwrap_err());
        assert!(text.contains("gh:not-an-issue"), "{text}");
        assert!(text.contains("owner/repo#number"), "{text}");
    }

    /// A thread of two messages, one of them read, under the schema `0002` left.
    const CONVERSATION_AT_READY: &str = "\
        INSERT INTO projects (id, slug, created_at) VALUES (1, 'widgets', 0);
        INSERT INTO members (project_id, id, handle, display_name, external_login, role, created_at)
            VALUES (1, 11, 'alice', 'Alice', NULL, 'admin', 0),
                   (1, 12, 'bob', 'Bob', NULL, 'member', 0);
        INSERT INTO threads (project_id, id, subject, item_id, author_id, to_project, created_at)
            VALUES (1, 81, 'Invoice totals', NULL, 11, 1, 10);
        INSERT INTO messages (project_id, id, thread_id, author_id, label, body, created_at)
            VALUES (1, 91, 81, 11, 'web', 'Can I take billing?', 20),
                   (1, 92, 81, 12, 'web', 'Yes.', 30);
        INSERT INTO reads (project_id, member_id, message_id, read_at) VALUES (1, 12, 91, 25);
    ";

    #[tokio::test]
    async fn acks_carry_every_message_over_as_normal_and_asking_for_nothing() {
        let (_dir, path) = database_at(2, CONVERSATION_AT_READY).await;
        let store = open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        let messages = tx.messages(ProjectId(1), ThreadId(81)).await.unwrap();
        let carried: Vec<(u64, &str, Importance, bool)> = messages
            .iter()
            .map(|m| (m.id.get(), m.body.as_str(), m.importance, m.ack_required))
            .collect();
        assert_eq!(
            carried,
            [
                (91, "Can I take billing?", Importance::Normal, false),
                (92, "Yes.", Importance::Normal, false),
            ]
        );
        let reads = tx.reads_of(ProjectId(1), MessageId(91)).await.unwrap();
        assert_eq!(reads.len(), 1, "the read marks are kept");
        assert!(
            tx.ack_requests(ProjectId(1), MemberId(12))
                .await
                .unwrap()
                .is_empty(),
            "nothing written before asks for an acknowledgement"
        );
        let summary = tx
            .thread_summaries(ProjectId(1), MemberId(12), None)
            .await
            .unwrap()
            .remove(0);
        assert_eq!(summary.importance, Importance::Normal);
        let ack = Ack {
            message: MessageId(91),
            member: MemberId(12),
            at: time::OffsetDateTime::UNIX_EPOCH,
        };
        tx.insert_ack(ProjectId(1), &ack).await.unwrap();
        tx.commit().await.unwrap();
        let mut tx = store.begin().await.unwrap();
        assert_eq!(tx.acks(ProjectId(1), MessageId(91)).await.unwrap(), [ack]);
    }

    #[tokio::test]
    async fn acks_refuse_an_importance_it_does_not_know() {
        let (_dir, path) = database_at(2, CONVERSATION_AT_READY).await;
        drop(open(&path).await.unwrap());
        let pool = SqlitePoolOptions::new()
            .connect_with(SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
        let refused = sqlx::query("UPDATE messages SET importance = 'loud' WHERE id = 91")
            .execute(&pool)
            .await;
        assert!(refused.is_err(), "the column keeps to the three names");
        let (version,): (i64,) = sqlx::query_as("SELECT max(version) FROM _sqlx_migrations")
            .fetch_one(&pool)
            .await
            .unwrap();
        let newest = MIGRATOR.iter().map(|m| m.version).max().unwrap();
        assert_eq!(version, newest, "every migration was applied");
    }

    /// Two claims, a conversation with an acknowledged message and a grant, under the schema `0003`
    /// left.
    const CLAIMS_AT_ACKS: &str = "\
        INSERT INTO projects (id, slug, created_at) VALUES (1, 'widgets', 0);
        INSERT INTO members (project_id, id, handle, display_name, external_login, role, created_at)
            VALUES (1, 11, 'alice', 'Alice', NULL, 'admin', 0),
                   (1, 12, 'bob', 'Bob', NULL, 'member', 0);
        INSERT INTO sessions (project_id, id, member_id, host, label, agent, push, last_seen)
            VALUES (1, 31, 11, 'laptop', 'laptop/mcp', 'agent', 0, 5),
                   (1, 32, 12, 'desktop', 'desktop/mcp', 'agent', 0, 5);
        INSERT INTO items (project_id, id, external_ref, title, status, repo, remote, created_by,
            created_at)
            VALUES (1, 51, 'gh:acme/widgets#1', 'Work', 'claimed', NULL, NULL, 11, 1),
                   (1, 52, NULL, 'More work', 'claimed', NULL, NULL, 12, 2);
        INSERT INTO claims (project_id, item_id, member_id, session_id, host, areas, claimed_at,
            renewed_at, last_seen)
            VALUES (1, 51, 11, 31, 'laptop', '[\"default:billing\"]', 10, 11, 12),
                   (1, 52, 12, 32, 'desktop', '[]', 10, 10, 10);
        INSERT INTO grants (project_id, id, item_id, repo, area, from_member, to_member, created_at)
            VALUES (1, 71, 52, 'default', 'billing', 11, 12, 13);
        INSERT INTO threads (project_id, id, subject, item_id, author_id, to_project, created_at)
            VALUES (1, 81, 'Invoice totals', 51, 11, 1, 20);
        INSERT INTO messages (project_id, id, thread_id, author_id, label, body, importance,
            ack_required, created_at)
            VALUES (1, 91, 81, 11, 'web', 'Please read.', 'high', 1, 21);
        INSERT INTO acks (project_id, message_id, member_id, at) VALUES (1, 91, 12, 22);
    ";

    #[tokio::test]
    async fn wide_carries_every_claim_over_as_ordinary_and_keeps_the_rest() {
        let (_dir, path) = database_at(3, CLAIMS_AT_ACKS).await;
        let store = open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        let claims = tx.claims(ProjectId(1)).await.unwrap();
        let carried: Vec<(u64, usize, bool)> = claims
            .iter()
            .map(|c| (c.item.get(), c.areas.len(), c.wide.is_none()))
            .collect();
        assert_eq!(carried, [(51, 1, true), (52, 0, true)]);
        assert_eq!(tx.grants(ProjectId(1)).await.unwrap().len(), 1);
        assert_eq!(tx.acks(ProjectId(1), MessageId(91)).await.unwrap().len(), 1);
        assert!(tx.open_wide_asks(ProjectId(1)).await.unwrap().is_empty());
        assert_eq!(
            tx.wide_ask(ProjectId(1), MessageId(91)).await.unwrap(),
            None
        );

        let reason = WideReason::parse("Rename the money type").unwrap();
        let areas = [QualifiedArea::parse("default:search").unwrap()].into();
        let widened = Claim {
            wide: Some(WideScope::new(areas, reason).unwrap()),
            ..claims[1].clone()
        };
        tx.save_claim(ProjectId(1), &widened).await.unwrap();
        let ask = WideAsk {
            message: MessageId(91),
            item: ItemId(52),
            from: MemberId(12),
            to: MemberId(11),
            areas: [QualifiedArea::parse("default:billing").unwrap()].into(),
        };
        tx.insert_wide_ask(ProjectId(1), &ask).await.unwrap();
        tx.commit().await.unwrap();
        let mut tx = store.begin().await.unwrap();
        assert_eq!(
            tx.claim(ProjectId(1), ItemId(52)).await.unwrap(),
            Some(widened)
        );
        assert_eq!(tx.open_wide_asks(ProjectId(1)).await.unwrap(), [ask]);
    }

    #[tokio::test]
    async fn wide_refuses_a_scope_without_its_reason_or_without_areas() {
        let (_dir, path) = database_at(3, CLAIMS_AT_ACKS).await;
        drop(open(&path).await.unwrap());
        let pool = SqlitePoolOptions::new()
            .connect_with(SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
        for update in [
            "UPDATE claims SET wide_areas = '[\"default:search\"]' WHERE item_id = 52",
            "UPDATE claims SET wide_reason = 'Rename' WHERE item_id = 52",
            "UPDATE claims SET wide_areas = '[]', wide_reason = 'Rename' WHERE item_id = 52",
            "UPDATE claims SET wide_areas = 'billing', wide_reason = 'Rename' WHERE item_id = 52",
        ] {
            assert!(
                sqlx::query(sqlx::AssertSqlSafe(update))
                    .execute(&pool)
                    .await
                    .is_err(),
                "{update}"
            );
        }
        let kept = sqlx::query(
            "UPDATE claims SET wide_areas = '[\"default:search\"]', wide_reason = 'Rename' \
             WHERE item_id = 52",
        )
        .execute(&pool)
        .await;
        assert!(kept.is_ok());
    }

    /// The rows of [`CLAIMS_AT_ACKS`] with one claim made wide, under the schema `0004` left.
    fn rows_at_wide() -> String {
        format!(
            "{CLAIMS_AT_ACKS}\n\
             UPDATE claims SET wide_areas = '[\"default:search\"]', wide_reason = 'Rename' \
             WHERE item_id = 52;\n\
             INSERT INTO wide_asks (project_id, message_id, item_id, from_id, to_id, areas) \
             VALUES (1, 91, 52, 12, 11, '[\"default:billing\"]');"
        )
    }

    #[tokio::test]
    async fn slots_start_empty_and_keep_every_row_of_before() {
        use dibs_domain::ids::{RunId, SessionId};
        use dibs_domain::session::HostName;
        use dibs_domain::slot::{Liveness, Purpose, SlotBoard, SlotHold, SlotName};
        use time::OffsetDateTime;

        let (_dir, path) = database_at(4, &rows_at_wide()).await;
        let store = open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        let claims = tx.claims(ProjectId(1)).await.unwrap();
        assert_eq!(claims.len(), 2);
        assert!(claims[1].wide.is_some(), "the wide claim is kept");
        assert_eq!(
            tx.open_wide_asks(ProjectId(1)).await.unwrap().len(),
            1,
            "still open"
        );
        assert_eq!(
            tx.wide_ask(ProjectId(1), MessageId(91))
                .await
                .unwrap()
                .map(|a| a.to),
            Some(MemberId(11))
        );
        assert!(tx.slot_board(ProjectId(1)).await.unwrap().is_empty());
        assert!(tx.busy_slot_projects().await.unwrap().is_empty());

        let at = OffsetDateTime::UNIX_EPOCH;
        let board = SlotBoard {
            holds: vec![SlotHold {
                slot: SlotName::parse("deploy:prod").unwrap(),
                member: MemberId(12),
                session: SessionId(32),
                host: HostName::parse("desktop").unwrap(),
                purpose: Purpose::parse("ship it").unwrap(),
                item: Some(ItemId(52)),
                liveness: Liveness::Run { run: RunId(5) },
                taken_at: at,
                expires_at: at,
                last_seen: at,
                stalled_at: None,
            }],
            ..SlotBoard::default()
        };
        tx.put_slot_board(ProjectId(1), &board).await.unwrap();
        tx.commit().await.unwrap();
        let mut tx = store.begin().await.unwrap();
        assert_eq!(tx.slot_board(ProjectId(1)).await.unwrap(), board);
    }

    #[tokio::test]
    async fn slots_refuse_a_hold_whose_liveness_columns_disagree() {
        let (_dir, path) = database_at(4, &rows_at_wide()).await;
        drop(open(&path).await.unwrap());
        let pool = SqlitePoolOptions::new()
            .connect_with(SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
        let insert = |liveness: &str, run: &str, workflow: &str, sha: &str, stalled: &str| {
            format!(
                "INSERT INTO slot_holds (project_id, slot, member_id, session_id, host, purpose, \
                 item_id, liveness, run_id, workflow, sha, taken_at, expires_at, last_seen, \
                 stalled_at) VALUES (1, 'deploy', 12, 32, 'desktop', 'ship', NULL, '{liveness}', \
                 {run}, {workflow}, {sha}, 0, 0, 0, {stalled})"
            )
        };
        for bad in [
            insert("run", "NULL", "NULL", "NULL", "NULL"),
            insert("session", "5", "NULL", "NULL", "NULL"),
            insert("until_run", "NULL", "'deploy.yml'", "NULL", "NULL"),
            insert("session", "NULL", "'deploy.yml'", "'abc1234'", "NULL"),
            insert("session", "NULL", "NULL", "NULL", "7"),
            insert("forever", "NULL", "NULL", "NULL", "NULL"),
        ] {
            assert!(
                sqlx::query(sqlx::AssertSqlSafe(bad.clone()))
                    .execute(&pool)
                    .await
                    .is_err(),
                "{bad}"
            );
        }
        for good in [insert("run", "5", "NULL", "NULL", "7")] {
            assert!(
                sqlx::query(sqlx::AssertSqlSafe(good.clone()))
                    .execute(&pool)
                    .await
                    .is_ok(),
                "{good}"
            );
        }
    }

    #[tokio::test]
    async fn deliveries_start_empty_and_keep_every_row_of_before() {
        use dibs_domain::delivery::{Delivery, DeliverySubject};
        use dibs_domain::ids::SessionId;
        use dibs_domain::slot::SlotName;
        use time::{Duration, OffsetDateTime};

        let rows = format!(
            "{}\n\
             INSERT INTO slot_holds (project_id, slot, member_id, session_id, host, purpose, \
             item_id, liveness, run_id, workflow, sha, taken_at, expires_at, last_seen, \
             stalled_at) VALUES (1, 'deploy:prod', 12, 32, 'desktop', 'ship it', NULL, \
             'session', NULL, NULL, NULL, 0, 600000000000, 0, NULL);\n\
             INSERT INTO slot_offers (project_id, slot, member_id, purpose, until) \
             VALUES (1, 'release', 11, 'tag 2.4', 300000000000);",
            rows_at_wide()
        );
        let (_dir, path) = database_at(5, &rows).await;
        let store = open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        let board = tx.slot_board(ProjectId(1)).await.unwrap();
        assert_eq!(board.holds.len(), 1, "the hold is kept");
        assert_eq!(board.offers.len(), 1, "the offer is kept");
        assert_eq!(tx.claims(ProjectId(1)).await.unwrap().len(), 2);
        let message = DeliverySubject::Message(MessageId(91));
        let offer = DeliverySubject::Offer {
            slot: SlotName::parse("release").unwrap(),
            until: OffsetDateTime::UNIX_EPOCH + Duration::minutes(5),
        };
        assert_eq!(
            tx.deliveries(
                ProjectId(1),
                MemberId(11),
                &[message.clone(), offer.clone()]
            )
            .await
            .unwrap(),
            [],
            "nothing was delivered before"
        );
        assert!(
            tx.raised_since(ProjectId(1), MemberId(12), OffsetDateTime::UNIX_EPOCH)
                .await
                .unwrap()
                .is_empty()
        );
        let delivered = Delivery {
            member: MemberId(11),
            subject: offer.clone(),
            session: SessionId(32),
            at: OffsetDateTime::UNIX_EPOCH,
        };
        tx.insert_delivery(ProjectId(1), &delivered).await.unwrap();
        tx.insert_raised(
            ProjectId(1),
            MemberId(12),
            MessageId(91),
            OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let mut tx = store.begin().await.unwrap();
        assert_eq!(
            tx.delivery(ProjectId(1), MemberId(11), &offer)
                .await
                .unwrap(),
            Some(delivered)
        );
        assert_eq!(
            tx.raised_since(ProjectId(1), MemberId(12), OffsetDateTime::UNIX_EPOCH)
                .await
                .unwrap(),
            [OffsetDateTime::UNIX_EPOCH]
        );
    }

    /// Every kind of row that refers to a thread or a message, under the schema `0006` left: a
    /// thread to bob with an announcement, a reply, a read mark, an acknowledgement, a counted
    /// message and a delivery; and a second thread to the project.
    fn rows_at_deliveries() -> String {
        format!(
            "{}\n\
             INSERT INTO thread_recipients (project_id, thread_id, member_id) VALUES (1, 81, 12);\n\
             UPDATE threads SET to_project = 0 WHERE id = 81;\n\
             INSERT INTO messages (project_id, id, thread_id, author_id, label, body, importance, \
             ack_required, created_at) VALUES (1, 92, 81, 12, 'desktop/mcp', 'On it.', 'normal', 0, 23);\n\
             INSERT INTO threads (project_id, id, subject, item_id, author_id, to_project, created_at) \
             VALUES (1, 82, 'Lunch', NULL, 12, 1, 24);\n\
             INSERT INTO messages (project_id, id, thread_id, author_id, label, body, importance, \
             ack_required, created_at) VALUES (1, 93, 82, 12, 'web', 'Pizza?', 'urgent', 0, 25);\n\
             INSERT INTO reads (project_id, member_id, message_id, read_at) VALUES (1, 11, 92, 26);\n\
             INSERT INTO raised_messages (project_id, message_id, member_id, at) VALUES (1, 93, 12, 25);\n\
             INSERT INTO message_deliveries (project_id, member_id, subject, session_id, at) \
             VALUES (1, 11, '93', 31, 27);",
            rows_at_wide()
        )
    }

    #[tokio::test]
    async fn people_keeps_every_thread_message_and_what_refers_to_them_and_lets_dibs_write() {
        use dibs_app::records::{MessageRecord, ThreadRecord};
        use dibs_domain::delivery::DeliverySubject;
        use dibs_domain::message::{Author, Body, Recipients, SessionLabel, Subject};
        use time::OffsetDateTime;

        let (_dir, path) = database_at(6, &rows_at_deliveries()).await;
        let store = open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        let project = ProjectId(1);
        let members = tx.members(project).await.unwrap();
        assert_eq!(members.len(), 2);
        assert!(
            members.iter().all(|m| m.removed_at.is_none()),
            "nobody was removed"
        );
        let thread = tx.thread(project, ThreadId(81)).await.unwrap().unwrap();
        assert_eq!(thread.author, Author::Member(MemberId(11)));
        assert_eq!(
            thread.recipients,
            Recipients::members([MemberId(12)].into()).unwrap()
        );
        let messages = tx.messages(project, ThreadId(81)).await.unwrap();
        let carried: Vec<(u64, Author, &str, Importance, bool)> = messages
            .iter()
            .map(|m| {
                (
                    m.id.get(),
                    m.author,
                    m.body.as_str(),
                    m.importance,
                    m.ack_required,
                )
            })
            .collect();
        assert_eq!(
            carried,
            [
                (
                    91,
                    Author::Member(MemberId(11)),
                    "Please read.",
                    Importance::High,
                    true
                ),
                (
                    92,
                    Author::Member(MemberId(12)),
                    "On it.",
                    Importance::Normal,
                    false
                ),
            ],
            "every message keeps its author, text, importance and request"
        );
        assert_eq!(tx.acks(project, MessageId(91)).await.unwrap().len(), 1);
        assert_eq!(tx.reads_of(project, MessageId(92)).await.unwrap().len(), 1);
        assert_eq!(
            tx.wide_ask(project, MessageId(91))
                .await
                .unwrap()
                .map(|a| a.to),
            Some(MemberId(11))
        );
        assert_eq!(
            tx.raised_since(project, MemberId(12), OffsetDateTime::UNIX_EPOCH)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            tx.delivery(
                project,
                MemberId(11),
                &DeliverySubject::Message(MessageId(93))
            )
            .await
            .unwrap()
            .is_some()
        );
        assert_eq!(tx.messages(project, ThreadId(82)).await.unwrap().len(), 1);
        assert!(tx.open_escalations(project).await.unwrap().is_empty());
        assert_eq!(tx.events_pruned_through(project).await.unwrap(), None);

        let at = OffsetDateTime::UNIX_EPOCH;
        let of_dibs = ThreadRecord {
            id: ThreadId(83),
            subject: Subject::parse("bob left the project").unwrap(),
            item: None,
            author: Author::System,
            recipients: Recipients::Project,
            created_at: at,
        };
        tx.insert_thread(project, &of_dibs).await.unwrap();
        let notice = MessageRecord {
            id: MessageId(94),
            thread: ThreadId(83),
            author: Author::System,
            label: SessionLabel::parse("dibs").unwrap(),
            body: Body::parse("Nothing was released.").unwrap(),
            importance: Importance::Normal,
            ack_required: false,
            created_at: at,
        };
        tx.insert_message(project, &notice).await.unwrap();
        tx.commit().await.unwrap();
        let mut tx = store.begin().await.unwrap();
        assert_eq!(
            tx.thread(project, ThreadId(83)).await.unwrap(),
            Some(of_dibs)
        );
        assert_eq!(
            tx.message(project, MessageId(94)).await.unwrap(),
            Some(notice)
        );
        assert_eq!(tx.delete_thread(project, ThreadId(81)).await.unwrap(), 2);
        tx.commit().await.unwrap();
        drop(store);

        let pool = SqlitePoolOptions::new()
            .connect_with(SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
        let broken: Vec<(String,)> =
            sqlx::query_as("SELECT \"table\" FROM pragma_foreign_key_check")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert!(
            broken.is_empty(),
            "no reference points at nothing: {broken:?}"
        );
        let indexes: Vec<(String,)> = sqlx::query_as(
            "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name IN ('threads', 'messages') \
             AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let names: Vec<&str> = indexes.iter().map(|(name,)| name.as_str()).collect();
        assert_eq!(
            names,
            [
                "messages_asking_ack",
                "messages_by_thread",
                "messages_by_thread_time",
                "threads_by_item"
            ],
            "the rebuilt tables keep their indexes"
        );
    }

    #[tokio::test]
    async fn review_carries_every_claim_over_as_worked_on_and_records_no_call() {
        use dibs_domain::review::{ClaimState, PrRef, Review};

        let (_dir, path) = database_at(7, &rows_at_deliveries()).await;
        let store = open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        let project = ProjectId(1);
        let claims = tx.claims(project).await.unwrap();
        assert_eq!(claims.len(), 2);
        assert!(
            claims
                .iter()
                .all(|claim| claim.state == ClaimState::Working),
            "every claim of before is worked on"
        );
        assert!(tx.last_calls(project).await.unwrap().is_empty());
        let since = claims[0].last_seen;
        let reviewing = Claim {
            state: ClaimState::InReview(
                Review::new(
                    [PrRef::parse("acme/widgets#7").unwrap()].into(),
                    since,
                    false,
                )
                .unwrap(),
            ),
            ..claims[0].clone()
        };
        tx.save_claim(project, &reviewing).await.unwrap();
        tx.commit().await.unwrap();
        let mut tx = store.begin().await.unwrap();
        assert_eq!(
            tx.claim(project, reviewing.item).await.unwrap(),
            Some(reviewing)
        );
        drop(tx);
        drop(store);

        let pool = SqlitePoolOptions::new()
            .connect_with(SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
        for update in [
            "UPDATE claims SET review_prs = '[\"acme/widgets#8\"]' WHERE item_id = 52",
            "UPDATE claims SET review_since = 5, review_more = 0 WHERE item_id = 52",
            "UPDATE claims SET review_prs = '[]', review_since = 5, review_more = 0 WHERE item_id = 52",
            "UPDATE claims SET review_prs = '[\"acme/widgets#8\"]', review_since = 5, review_more = 2 \
             WHERE item_id = 52",
        ] {
            assert!(
                sqlx::query(sqlx::AssertSqlSafe(update))
                    .execute(&pool)
                    .await
                    .is_err(),
                "{update}"
            );
        }
        let kept = sqlx::query(
            "UPDATE claims SET review_prs = '[\"acme/widgets#8\"]', review_since = 5, \
             review_more = 1 WHERE item_id = 52",
        )
        .execute(&pool)
        .await;
        assert!(kept.is_ok());
    }
}
