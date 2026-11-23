//! Each migration on a database the one before it created, with rows in it: the rows are still read
//! through the store afterwards, and what a migration refuses to carry over fails it, saying what.

#[cfg(test)]
mod migrations {
    use std::path::{Path, PathBuf};

    use sqlx::migrate::Migrator;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use tempfile::TempDir;

    use dibs_app::ports::Store;
    use dibs_app::records::{AskedGrantRecord, ItemFilter};
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

    /// The requests put to `holder` that they have not answered, without whether dibs left each to
    /// them.
    async fn unanswered(
        tx: &mut Box<dyn dibs_app::ports::Tx>,
        project: ProjectId,
        holder: MemberId,
    ) -> Vec<dibs_domain::grant::AskedGrant> {
        tx.unanswered_asked_grants(project, holder)
            .await
            .unwrap()
            .into_iter()
            .map(|record| record.asked)
            .collect()
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
            acked_by_dibs: None,
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

    #[tokio::test]
    async fn radar_carries_every_claim_over_with_no_branch_and_starts_with_no_footprint() {
        use dibs_domain::radar::{BranchName, BranchSource, RecordedBranch};
        use dibs_domain::repo::RepoId;

        let (_dir, path) = database_at(8, &rows_at_deliveries()).await;
        let store = open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        let project = ProjectId(1);
        let claims = tx.claims(project).await.unwrap();
        assert_eq!(claims.len(), 2);
        assert!(
            claims.iter().all(|claim| claim.branches.is_empty()),
            "no claim of before records a branch"
        );
        assert!(tx.footprints(project).await.unwrap().is_empty());
        assert!(tx.overlaps(project).await.unwrap().is_empty());
        let branched = Claim {
            branches: [(
                RepoId::implicit(),
                RecordedBranch::new(BranchName::parse("alice/51").unwrap(), BranchSource::Named),
            )]
            .into(),
            ..claims[0].clone()
        };
        tx.save_claim(project, &branched).await.unwrap();
        tx.commit().await.unwrap();
        let mut tx = store.begin().await.unwrap();
        assert_eq!(
            tx.claim(project, branched.item).await.unwrap(),
            Some(branched)
        );
        drop(tx);
        drop(store);

        let pool = SqlitePoolOptions::new()
            .connect_with(SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
        for update in [
            "UPDATE claims SET branches = '[]' WHERE item_id = 52",
            "UPDATE claims SET branches = 'main' WHERE item_id = 52",
        ] {
            assert!(
                sqlx::query(sqlx::AssertSqlSafe(update))
                    .execute(&pool)
                    .await
                    .is_err(),
                "{update}"
            );
        }
    }

    #[tokio::test]
    async fn yield_carries_every_claim_over_as_a_feature_waiting_for_nothing() {
        use dibs_domain::handover::{ClaimKind, PendingArea};

        let (_dir, path) = database_at(9, &rows_at_deliveries()).await;
        let store = open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        let project = ProjectId(1);
        let claims = tx.claims(project).await.unwrap();
        assert_eq!(claims.len(), 2);
        assert!(
            claims
                .iter()
                .all(|claim| claim.kind == ClaimKind::Feature && claim.pending.is_empty()),
            "every claim of before is a feature that waits for nothing"
        );
        assert!(tx.pending_projects().await.unwrap().is_empty());
        let debt = Claim {
            kind: ClaimKind::Debt,
            ..claims[1].clone()
        };
        tx.save_claim(project, &debt).await.unwrap();
        let mut waiting = claims[0].clone();
        waiting.pending.insert(
            QualifiedArea::parse("default:billing").unwrap(),
            PendingArea::waiting_for(debt.item),
        );
        tx.save_claim(project, &waiting).await.unwrap();
        tx.commit().await.unwrap();
        let mut tx = store.begin().await.unwrap();
        assert_eq!(
            tx.claim(project, waiting.item).await.unwrap(),
            Some(waiting.clone())
        );
        drop(tx);
        drop(store);

        let pool = SqlitePoolOptions::new()
            .connect_with(SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
        for update in [
            "UPDATE claims SET kind = 'chore' WHERE item_id = 52",
            "UPDATE claim_pending SET from_item_id = item_id",
            "UPDATE claim_pending SET escalated_at = 5, asked_at = NULL",
        ] {
            assert!(
                sqlx::query(sqlx::AssertSqlSafe(update))
                    .execute(&pool)
                    .await
                    .is_err(),
                "{update}"
            );
        }
    }

    #[tokio::test]
    async fn reports_start_empty_on_a_database_of_before_and_refuse_a_malformed_row() {
        use std::collections::BTreeMap;

        use dibs_domain::report::{DayCounts, DayReport, HookReport, ReportDay, Tally};
        use dibs_domain::review::PrRef;
        use dibs_domain::session::HostName;

        let (_dir, path) = database_at(10, &rows_at_deliveries()).await;
        let store = open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        let project = ProjectId(1);
        let day = ReportDay::parse("2026-03-02").unwrap();
        assert!(tx.hook_reports(project, day, day).await.unwrap().is_empty());
        let report = HookReport {
            member: MemberId(11),
            machine: dibs_domain::report::MachineId::from_bits(1),
            host: HostName::parse("laptop").unwrap(),
            report: DayReport {
                day,
                counts: DayCounts::new(BTreeMap::from([(Tally::Frozen, 1)]), Vec::new()).unwrap(),
            },
        };
        tx.save_hook_report(project, &report).await.unwrap();
        let pr = PrRef::parse("acme/widgets#1").unwrap();
        tx.save_pr_mergeable(project, &pr, false, time::OffsetDateTime::UNIX_EPOCH)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let mut tx = store.begin().await.unwrap();
        assert_eq!(tx.hook_reports(project, day, day).await.unwrap(), [report]);
        assert_eq!(tx.pr_mergeable(project, &pr).await.unwrap(), Some(false));
        drop(tx);
        drop(store);

        let pool = SqlitePoolOptions::new()
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .foreign_keys(true),
            )
            .await
            .unwrap();
        for update in [
            "UPDATE hook_reports SET day = '2026-3-2'",
            "UPDATE hook_reports SET counts = '[]'",
            "UPDATE hook_reports SET durations = 'not json'",
            "UPDATE hook_reports SET machine = 'laptop'",
            "UPDATE hook_reports SET machine = upper(machine) || 'A'",
            "UPDATE pr_states SET mergeable = 2",
        ] {
            assert!(
                sqlx::query(sqlx::AssertSqlSafe(update))
                    .execute(&pool)
                    .await
                    .is_err(),
                "{update}"
            );
        }
    }

    #[tokio::test]
    async fn grant_asks_start_empty_on_a_database_of_before_and_refuse_a_dangling_row() {
        use dibs_domain::grant::{AskedGrant, GrantAsk};

        let (_dir, path) = database_at(11, &rows_at_deliveries()).await;
        let store = open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        let project = ProjectId(1);
        let alice = MemberId(11);
        assert!(
            tx.unanswered_asked_grants(project, alice)
                .await
                .unwrap()
                .is_empty()
        );
        let asked = AskedGrant {
            message: MessageId(93),
            ask: GrantAsk {
                asker: MemberId(12),
                item: ItemId(52),
                area: QualifiedArea::parse("default:billing").unwrap(),
            },
            holder: alice,
        };
        tx.insert_asked_grant(project, &asked).await.unwrap();
        tx.commit().await.unwrap();
        let mut tx = store.begin().await.unwrap();
        assert_eq!(unanswered(&mut tx, project, alice).await, [asked]);
        drop(tx);
        drop(store);

        let pool = SqlitePoolOptions::new()
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .foreign_keys(true),
            )
            .await
            .unwrap();
        for update in [
            "UPDATE grant_asks SET holder_id = 99",
            "UPDATE grant_asks SET item_id = 99",
            "UPDATE grant_asks SET message_id = 99",
            "UPDATE grant_asks SET asker_id = 'bob'",
        ] {
            assert!(
                sqlx::query(sqlx::AssertSqlSafe(update))
                    .execute(&pool)
                    .await
                    .is_err(),
                "{update}"
            );
        }
    }

    #[tokio::test]
    async fn worktree_holds_start_empty_on_a_database_of_before_and_refuse_a_holder_twice() {
        use std::collections::BTreeMap;

        use dibs_domain::grant::{AskedGrant, GrantAsk};
        use dibs_domain::ids::SessionId;
        use dibs_domain::report::{DayCounts, MachineId, ReportDay, Tally};
        use dibs_domain::session::HostName;
        use dibs_domain::slot::Purpose;
        use dibs_domain::worktree::{
            HoldItem, Holder, PoolId, PoolKey, WorktreeHold, WorktreeName, WorktreePool,
        };
        use time::OffsetDateTime;

        let rows = format!(
            "{}\n\
             INSERT INTO slot_holds (project_id, slot, member_id, session_id, host, purpose, \
             item_id, liveness, run_id, workflow, sha, taken_at, expires_at, last_seen, \
             stalled_at) VALUES (1, 'deploy:prod', 12, 32, 'desktop', 'ship it', NULL, \
             'session', NULL, NULL, NULL, 0, 600000000000, 0, NULL);\n\
             INSERT INTO hook_reports (project_id, day, member_id, machine, host, counts, \
             durations) VALUES (1, '2026-03-02', 11, '{:032x}', 'laptop', '{{\"frozen\": 1}}', \
             '[]');\n\
             INSERT INTO grant_asks (project_id, message_id, asker_id, item_id, repo, area, \
             holder_id) VALUES (1, 93, 12, 52, 'default', 'billing', 11);",
            rows_at_deliveries(),
            1
        );
        let (_dir, path) = database_at(12, &rows).await;
        let store = open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        let project = ProjectId(1);
        let alice = MemberId(11);
        assert_eq!(tx.claims(project).await.unwrap().len(), 2);
        assert_eq!(tx.slot_board(project).await.unwrap().holds.len(), 1);
        let day = ReportDay::parse("2026-03-02").unwrap();
        let reports = tx.hook_reports(project, day, day).await.unwrap();
        assert_eq!(
            reports
                .iter()
                .map(|r| r.report.counts.clone())
                .collect::<Vec<_>>(),
            [DayCounts::new(BTreeMap::from([(Tally::Frozen, 1)]), Vec::new()).unwrap()]
        );
        assert_eq!(
            unanswered(&mut tx, project, alice).await,
            [AskedGrant {
                message: MessageId(93),
                ask: GrantAsk {
                    asker: MemberId(12),
                    item: ItemId(52),
                    area: QualifiedArea::parse("default:billing").unwrap(),
                },
                holder: alice,
            }]
        );
        assert!(tx.worktree_pools(project).await.unwrap().is_empty());
        assert!(tx.busy_worktree_pools().await.unwrap().is_empty());
        let key = PoolKey {
            member: alice,
            machine: MachineId::from_bits(1),
            pool: PoolId::parse("a1").unwrap(),
        };
        let pool = WorktreePool::new(
            key.clone(),
            vec![WorktreeHold {
                worktree: WorktreeName::parse("worktree1").unwrap(),
                holder: Holder::Session(SessionId(31)),
                host: HostName::parse("laptop").unwrap(),
                purpose: Purpose::parse("fix the totals").unwrap(),
                item: HoldItem::Named(ItemId(51)),
                taken_at: OffsetDateTime::UNIX_EPOCH,
                last_seen: OffsetDateTime::UNIX_EPOCH,
            }],
        )
        .unwrap();
        tx.put_worktree_pool(project, &pool).await.unwrap();
        tx.commit().await.unwrap();
        let mut tx = store.begin().await.unwrap();
        assert_eq!(tx.worktree_pool(project, &key).await.unwrap(), pool);
        assert_eq!(tx.busy_worktree_pools().await.unwrap(), [(project, key)]);
        drop(tx);
        drop(store);

        let pool = SqlitePoolOptions::new()
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .foreign_keys(true),
            )
            .await
            .unwrap();
        let insert = |machine: &str, pool: &str, worktree: &str, session: u64, agent: &str| {
            format!(
                "INSERT INTO worktree_holds (project_id, member_id, machine, pool, worktree, \
                 session_id, agent, host, purpose, item_id, taken_at, last_seen) VALUES (1, 11, \
                 '{machine}', '{pool}', '{worktree}', {session}, {agent}, 'laptop', 'work', \
                 NULL, 0, 0)"
            )
        };
        let laptop = format!("{:032x}", 1);
        for (what, bad) in [
            (
                "the worktree held twice",
                insert(&laptop, "a1", "worktree1", 32, "NULL"),
            ),
            (
                "the session holding a second worktree",
                insert(&laptop, "a1", "worktree2", 31, "NULL"),
            ),
            (
                "an empty agent",
                insert(&laptop, "a1", "worktree2", 32, "''"),
            ),
            (
                "a machine that is no id",
                insert("laptop", "a1", "worktree2", 32, "NULL"),
            ),
            (
                "a pool that is no id",
                insert(&laptop, "A-1", "worktree2", 32, "NULL"),
            ),
            (
                "a missing session",
                insert(&laptop, "a1", "worktree2", 99, "NULL"),
            ),
        ] {
            assert!(
                sqlx::query(sqlx::AssertSqlSafe(bad))
                    .execute(&pool)
                    .await
                    .is_err(),
                "{what}"
            );
        }
        for (what, good) in [
            (
                "an agent of the holding session",
                insert(&laptop, "a1", "worktree2", 31, "'agent-1'"),
            ),
            (
                "another agent of it",
                insert(&laptop, "a1", "worktree3", 31, "'agent-2'"),
            ),
            (
                "the session in another pool of the machine",
                insert(&laptop, "a2", "worktree1", 31, "NULL"),
            ),
        ] {
            assert!(
                sqlx::query(sqlx::AssertSqlSafe(good))
                    .execute(&pool)
                    .await
                    .is_ok(),
                "{what}"
            );
        }
        assert!(
            sqlx::query(sqlx::AssertSqlSafe(insert(
                &laptop,
                "a1",
                "worktree4",
                31,
                "'agent-1'"
            )))
            .execute(&pool)
            .await
            .is_err(),
            "the agent holding a second worktree"
        );
        for update in [
            "UPDATE worktree_holds SET item_id = 99",
            "UPDATE worktree_holds SET member_id = 99",
        ] {
            assert!(
                sqlx::query(sqlx::AssertSqlSafe(update))
                    .execute(&pool)
                    .await
                    .is_err(),
                "{update}"
            );
        }
    }

    #[tokio::test]
    async fn branch_sources_forget_every_branch_and_what_the_radar_derived_and_keep_the_rest() {
        use dibs_domain::event::EventKind;
        use dibs_domain::radar::{BranchName, BranchSourceKind};
        use dibs_domain::repo::RepoId;
        use dibs_domain::session::{CheckoutToken, SessionCheckout};
        use time::OffsetDateTime;

        let rows = format!(
            "{}\n\
             UPDATE claims SET branches = '{{\"default\": \"alice/51-rates\", \"web\": \"main\"}}' \
             WHERE item_id = 51;\n\
             UPDATE claims SET branches = '{{\"default\": \"bob/52-more\"}}' WHERE item_id = 52;\n\
             INSERT INTO footprints (project_id, item_id, repo, branch, base_branch, base_sha, \
             base_at, truncated, path_count, digest) VALUES \
             (1, 51, 'default', 'alice/51-rates', 'main', 'abcd', 0, 0, 1, \
             '{{\"areas\": [], \"unzoned\": true, \"contracts\": []}}'), \
             (1, 52, 'default', 'bob/52-more', 'main', 'abcd', 0, 0, 1, \
             '{{\"areas\": [], \"unzoned\": true, \"contracts\": []}}');\n\
             INSERT INTO footprint_paths (project_id, item_id, repo, path) VALUES \
             (1, 51, 'default', 'config/app.yaml'), (1, 52, 'default', 'config/app.yaml');\n\
             INSERT INTO overlaps (project_id, overlap_key, kind, pr, merged_at, paths) VALUES \
             (1, 'same_file 51 52', 'same_file', NULL, NULL, '[\"default:config/app.yaml\"]');\n\
             INSERT INTO overlap_items (project_id, overlap_key, item_id) VALUES \
             (1, 'same_file 51 52', 51), (1, 'same_file 51 52', 52);\n\
             INSERT INTO events (project_id, kind, payload, at) VALUES (1, 'branch_recorded', \
             '{{\"item\": 51, \"member\": 11, \"repo\": \"default\", \
             \"branch\": \"alice/51-rates\"}}', 30);\n\
             INSERT INTO worktree_holds (project_id, member_id, machine, pool, worktree, \
             session_id, agent, host, purpose, item_id, taken_at, last_seen) VALUES \
             (1, 11, '{laptop}', 'a1', 'worktree1', 31, NULL, 'laptop', 'fix the totals', 51, 0, 0), \
             (1, 11, '{laptop}', 'a1', 'worktree2', 32, NULL, 'laptop', 'look around', NULL, 0, 0);",
            rows_at_deliveries(),
            laptop = format!("{:032x}", 1)
        );
        let (_dir, path) = database_at(13, &rows).await;
        let store = open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        let project = ProjectId(1);
        let claims = tx.claims(project).await.unwrap();
        let carried: Vec<(u64, usize, usize)> = claims
            .iter()
            .map(|claim| (claim.item.get(), claim.areas.len(), claim.branches.len()))
            .collect();
        assert_eq!(
            carried,
            [(51, 1, 0), (52, 0, 0)],
            "every claim stays with its areas, and no branch recorded before"
        );
        assert!(tx.footprints(project).await.unwrap().is_empty());
        let paths = [dibs_domain::paths::RelPath::parse("config/app.yaml").unwrap()].into();
        assert!(
            tx.footprint_entries(project, &RepoId::implicit(), &paths)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(tx.overlaps(project).await.unwrap().is_empty());
        let events = tx.events_after(project, None, 10).await.unwrap();
        let [event] = events.as_slice() else {
            panic!("the log keeps its events: {events:?}");
        };
        let EventKind::BranchRecorded(recorded) = &event.kind else {
            panic!("the branch recorded before: {event:?}");
        };
        assert_eq!(
            (recorded.branch.as_str(), recorded.source),
            ("alice/51-rates", BranchSourceKind::Checkout),
            "an event stored without a source reads as a checkout's"
        );
        assert!(tx.thread(project, ThreadId(81)).await.unwrap().is_some());
        assert_eq!(tx.messages(project, ThreadId(81)).await.unwrap().len(), 2);
        let alice = MemberId(11);
        assert!(
            tx.session_checkouts(project, alice, OffsetDateTime::UNIX_EPOCH)
                .await
                .unwrap()
                .is_empty()
        );
        let branched = Claim {
            branches: [(
                RepoId::implicit(),
                dibs_domain::radar::RecordedBranch::new(
                    BranchName::parse("alice/51-rates").unwrap(),
                    dibs_domain::radar::BranchSource::Checkout,
                ),
            )]
            .into(),
            ..claims[0].clone()
        };
        tx.save_claim(project, &branched).await.unwrap();
        let reported = SessionCheckout {
            session: dibs_domain::ids::SessionId(31),
            member: alice,
            checkout: CheckoutToken::parse(&format!("{:032x}", 5)).unwrap(),
            reported_at: OffsetDateTime::UNIX_EPOCH,
        };
        tx.save_session_checkout(project, &reported).await.unwrap();
        tx.commit().await.unwrap();
        let mut tx = store.begin().await.unwrap();
        assert_eq!(
            tx.claim(project, branched.item).await.unwrap(),
            Some(branched)
        );
        assert_eq!(
            tx.session_checkouts(project, alice, OffsetDateTime::UNIX_EPOCH)
                .await
                .unwrap(),
            [reported]
        );
        drop(tx);
        drop(store);

        let pool = SqlitePoolOptions::new()
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .foreign_keys(true),
            )
            .await
            .unwrap();
        let insert = |checkout: &str| {
            format!(
                "INSERT INTO session_checkouts (project_id, session_id, member_id, checkout, \
                 reported_at) VALUES (1, 32, 12, '{checkout}', 0)"
            )
        };
        let token = format!("{:032x}", 6);
        for (what, bad) in [
            ("an uppercase token", token.to_uppercase().replace('0', "A")),
            ("a token one character short", token[1..].to_owned()),
            ("a token one character long", format!("{token}0")),
            (
                "a token that is no hexadecimal",
                format!("{}g", &token[1..]),
            ),
        ] {
            assert!(
                sqlx::query(sqlx::AssertSqlSafe(insert(&bad)))
                    .execute(&pool)
                    .await
                    .is_err(),
                "{what}"
            );
        }
        assert!(
            sqlx::query(sqlx::AssertSqlSafe(insert(&token)))
                .execute(&pool)
                .await
                .is_ok()
        );
        let missing_session = format!(
            "INSERT INTO session_checkouts (project_id, session_id, member_id, checkout, \
             reported_at) VALUES (1, 99, 12, '{token}', 0)"
        );
        assert!(
            sqlx::query(sqlx::AssertSqlSafe(missing_session))
                .execute(&pool)
                .await
                .is_err(),
            "a checkout of a missing session"
        );
        for (what, bad) in [
            (
                "an item learned without an item",
                "UPDATE worktree_holds SET item_learned = 1 WHERE worktree = 'worktree2'",
            ),
            (
                "a flag neither 0 nor 1",
                "UPDATE worktree_holds SET item_learned = 2 WHERE worktree = 'worktree1'",
            ),
        ] {
            assert!(sqlx::query(bad).execute(&pool).await.is_err(), "{what}");
        }
    }

    #[tokio::test]
    async fn a_holds_item_reads_as_named_from_before_and_keeps_whether_it_was_learned() {
        use dibs_app::ports::StoreError;
        use dibs_domain::ids::SessionId;
        use dibs_domain::report::MachineId;
        use dibs_domain::session::HostName;
        use dibs_domain::slot::Purpose;
        use dibs_domain::worktree::{
            HoldItem, Holder, PoolId, PoolKey, WorktreeHold, WorktreeName, WorktreePool,
        };
        use time::OffsetDateTime;

        let rows = format!(
            "{}\n\
             INSERT INTO worktree_holds (project_id, member_id, machine, pool, worktree, \
             session_id, agent, host, purpose, item_id, taken_at, last_seen) VALUES \
             (1, 11, '{laptop}', 'a1', 'worktree1', 31, NULL, 'laptop', 'fix the totals', 51, 0, 0), \
             (1, 11, '{laptop}', 'a1', 'worktree2', 32, NULL, 'laptop', 'look around', NULL, 0, 0);",
            rows_at_deliveries(),
            laptop = format!("{:032x}", 1)
        );
        let (_dir, path) = database_at(13, &rows).await;
        let store = open(&path).await.unwrap();
        let project = ProjectId(1);
        let key = PoolKey {
            member: MemberId(11),
            machine: MachineId::from_bits(1),
            pool: PoolId::parse("a1").unwrap(),
        };
        let mut tx = store.begin().await.unwrap();
        let before = tx.worktree_pool(project, &key).await.unwrap();
        let items: Vec<(&str, HoldItem)> = before
            .holds()
            .iter()
            .map(|hold| (hold.worktree.as_str(), hold.item))
            .collect();
        assert_eq!(
            items,
            [
                ("worktree1", HoldItem::Named(ItemId(51))),
                ("worktree2", HoldItem::Unknown),
            ],
            "only a take named an item before"
        );
        let hold = |worktree: &str, session: u64, item: HoldItem| WorktreeHold {
            worktree: WorktreeName::parse(worktree).unwrap(),
            holder: Holder::Session(SessionId(session)),
            host: HostName::parse("laptop").unwrap(),
            purpose: Purpose::parse("fix the totals").unwrap(),
            item,
            taken_at: OffsetDateTime::UNIX_EPOCH,
            last_seen: OffsetDateTime::UNIX_EPOCH,
        };
        let learned = WorktreePool::new(
            key.clone(),
            vec![
                hold("worktree1", 31, HoldItem::Named(ItemId(51))),
                hold("worktree2", 32, HoldItem::Learned(ItemId(52))),
            ],
        )
        .unwrap();
        tx.put_worktree_pool(project, &learned).await.unwrap();
        tx.commit().await.unwrap();
        drop(store);
        let store = open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        assert_eq!(
            tx.worktree_pool(project, &key).await.unwrap(),
            learned,
            "read back from the file"
        );
        drop(tx);
        drop(store);

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
        sqlx::query("PRAGMA ignore_check_constraints = ON")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE worktree_holds SET item_id = NULL WHERE worktree = 'worktree2'")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
        let store = open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        assert!(matches!(
            tx.worktree_pool(project, &key).await,
            Err(StoreError::Corrupt { what }) if what.starts_with("worktree_holds.item_learned")
        ));
    }

    #[tokio::test]
    async fn a_claims_areas_read_as_declared_from_before_and_keep_what_an_edit_gained() {
        use dibs_app::ports::StoreError;
        use dibs_domain::claim::AreaOrigin;

        let (_dir, path) = database_at(13, &rows_at_deliveries()).await;
        let store = open(&path).await.unwrap();
        let project = ProjectId(1);
        let mut tx = store.begin().await.unwrap();
        let claims = tx.claims(project).await.unwrap();
        let billing = QualifiedArea::parse("default:billing").unwrap();
        assert_eq!(claims[0].areas.origin(&billing), Some(AreaOrigin::Declared));
        assert!(
            claims.iter().all(|claim| claim.areas.gained().is_empty()),
            "every area of before was declared"
        );
        let search = QualifiedArea::parse("default:search").unwrap();
        let mut gaining = claims[0].clone();
        gaining
            .areas
            .gain(search.clone(), time::OffsetDateTime::UNIX_EPOCH);
        tx.save_claim(project, &gaining).await.unwrap();
        tx.commit().await.unwrap();
        drop(store);
        let store = open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        let read = tx.claim(project, gaining.item).await.unwrap().unwrap();
        assert_eq!(read, gaining, "read back from the file");
        assert_eq!(read.areas.origin(&search), Some(AreaOrigin::Gained));
        drop(tx);
        drop(store);

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
        for (what, bad) in [
            ("no JSON", "'default:search'"),
            ("no array", "'{}'"),
            ("nothing", "NULL"),
        ] {
            let update = format!("UPDATE claims SET gained_areas = {bad} WHERE item_id = 51");
            assert!(
                sqlx::query(sqlx::AssertSqlSafe(update))
                    .execute(&pool)
                    .await
                    .is_err(),
                "{what}"
            );
        }
        sqlx::query("UPDATE claims SET gained_areas = '[\"default:checkout\"]' WHERE item_id = 51")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
        let store = open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        assert!(matches!(
            tx.claim(project, ItemId(51)).await,
            Err(StoreError::Corrupt { what }) if what.starts_with("claims.gained_areas")
        ));
    }

    #[tokio::test]
    async fn every_area_held_before_starts_its_clock_at_the_upgrade_and_a_held_area_needs_one() {
        use dibs_app::ports::StoreError;
        use time::{Duration, OffsetDateTime};

        let rows = format!(
            "{}\n\
             UPDATE claims SET areas = '[\"default:billing\",\"default:search\"]', \
             gained_areas = '[\"default:search\"]' WHERE item_id = 51;",
            rows_at_deliveries()
        );
        let (_dir, path) = database_at(15, &rows).await;
        let before = OffsetDateTime::now_utc() - Duration::milliseconds(1);
        let store = open(&path).await.unwrap();
        let after = OffsetDateTime::now_utc();
        let project = ProjectId(1);
        let mut tx = store.begin().await.unwrap();
        let claims = tx.claims(project).await.unwrap();
        let billing = QualifiedArea::parse("default:billing").unwrap();
        let search = QualifiedArea::parse("default:search").unwrap();
        let held = &claims[0].areas;
        assert_eq!(held.gained(), &[search.clone()].into(), "the origins stay");
        let clocks: Vec<&QualifiedArea> = held.clocks().keys().collect();
        assert_eq!(clocks, [&billing, &search], "a clock for every held area");
        for (area, clock) in held.clocks() {
            assert!(
                (before..=after).contains(clock),
                "{area} starts at the upgrade, not at the claim: {clock} outside {before}..{after}"
            );
        }
        assert_eq!(
            held.clock(&billing),
            held.clock(&search),
            "one upgrade, one time"
        );
        assert!(claims[1].areas.clocks().is_empty(), "a claim with no area");
        let mut moved = claims[0].clone();
        moved.areas.note_edit(&billing, OffsetDateTime::UNIX_EPOCH);
        tx.save_claim(project, &moved).await.unwrap();
        tx.commit().await.unwrap();
        drop(store);
        let store = open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        assert_eq!(
            tx.claim(project, ItemId(51)).await.unwrap(),
            Some(moved),
            "read back from the file"
        );
        drop(tx);
        drop(store);

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
        for (what, bad) in [
            ("no JSON", "'default:billing'"),
            ("no object", "'[]'"),
            ("nothing", "NULL"),
        ] {
            let update = format!("UPDATE claims SET area_clocks = {bad} WHERE item_id = 51");
            assert!(
                sqlx::query(sqlx::AssertSqlSafe(update))
                    .execute(&pool)
                    .await
                    .is_err(),
                "{what}"
            );
        }
        sqlx::query(
            "UPDATE claims SET area_clocks = '{\"default:billing\": 0}' WHERE item_id = 51",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
        let store = open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        assert!(matches!(
            tx.claim(project, ItemId(51)).await,
            Err(StoreError::Corrupt { what }) if what.starts_with("claims.area_clocks")
        ));
    }

    #[tokio::test]
    async fn claim_threads_start_empty_keep_every_thread_of_before_and_refuse_a_dangling_row() {
        use dibs_app::records::ClaimThread;
        use dibs_domain::claim::ClaimInstance;

        let (_dir, path) = database_at(14, &rows_at_deliveries()).await;
        let store = open(&path).await.unwrap();
        let project = ProjectId(1);
        let mut tx = store.begin().await.unwrap();
        let instance = ClaimInstance {
            item: ItemId(51),
            member: MemberId(12),
            claimed_at: time::OffsetDateTime::UNIX_EPOCH,
        };
        assert_eq!(tx.claim_thread(project, &instance).await.unwrap(), None);
        assert!(
            tx.thread(project, ThreadId(81)).await.unwrap().is_some(),
            "a conversation of before stays, linked to no claim"
        );
        let link = ClaimThread {
            thread: ThreadId(81),
            claim: instance,
            base_moved_told_at: None,
        };
        tx.insert_claim_thread(project, &link).await.unwrap();
        tx.commit().await.unwrap();
        let mut tx = store.begin().await.unwrap();
        assert_eq!(
            tx.claim_thread(project, &instance).await.unwrap(),
            Some(link)
        );
        drop(tx);
        drop(store);

        let pool = SqlitePoolOptions::new()
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .foreign_keys(true),
            )
            .await
            .unwrap();
        for update in [
            "UPDATE claim_threads SET thread_id = 99",
            "UPDATE claim_threads SET item_id = 99",
            "UPDATE claim_threads SET member_id = 99",
            "UPDATE claim_threads SET claimed_at = 'soon'",
        ] {
            assert!(
                sqlx::query(sqlx::AssertSqlSafe(update))
                    .execute(&pool)
                    .await
                    .is_err(),
                "{update}"
            );
        }
    }

    #[tokio::test]
    async fn every_session_of_before_is_open_and_an_ending_is_kept() {
        use dibs_domain::ids::SessionId;
        use time::OffsetDateTime;

        let (_dir, path) = database_at(16, &rows_at_deliveries()).await;
        let store = open(&path).await.unwrap();
        let project = ProjectId(1);
        let mut tx = store.begin().await.unwrap();
        let mut laptop = tx.session(project, SessionId(31)).await.unwrap().unwrap();
        let desktop = tx.session(project, SessionId(32)).await.unwrap().unwrap();
        assert!(
            laptop.is_open() && desktop.is_open(),
            "{laptop:?} {desktop:?}"
        );
        assert_eq!(laptop.label.as_str(), "laptop/mcp", "the row is kept");
        assert_eq!(
            laptop.last_seen,
            OffsetDateTime::UNIX_EPOCH + time::Duration::nanoseconds(5)
        );
        assert_eq!(
            tx.members_seen_since(project, OffsetDateTime::UNIX_EPOCH)
                .await
                .unwrap(),
            [MemberId(11), MemberId(12)].into(),
            "both open sessions count as seen"
        );
        laptop.end(OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(9));
        tx.save_session(project, &laptop).await.unwrap();
        tx.commit().await.unwrap();
        drop(store);
        let store = open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        assert_eq!(
            tx.session(project, SessionId(31)).await.unwrap(),
            Some(laptop),
            "read back from the file"
        );
        assert_eq!(
            tx.members_seen_since(project, OffsetDateTime::UNIX_EPOCH)
                .await
                .unwrap(),
            [MemberId(12)].into()
        );
        drop(tx);
        drop(store);

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
        assert!(
            sqlx::query("UPDATE sessions SET ended_at = 'soon' WHERE id = 31")
                .execute(&pool)
                .await
                .is_err(),
            "the column keeps to times"
        );
    }

    #[tokio::test]
    async fn server_decisions_read_every_row_of_before_as_decided_by_people_and_refuse_half_a_decision()
     {
        use dibs_app::ports::StoreError;
        use dibs_domain::grant::{AskedGrant, GrantAsk, GrantBlock, GrantDecision};
        use time::{Duration, OffsetDateTime};

        let rows = format!(
            "{}\n\
             INSERT INTO grant_asks (project_id, message_id, asker_id, item_id, repo, area, \
             holder_id) VALUES (1, 93, 12, 52, 'default', 'billing', 11);\n\
             INSERT INTO overlaps (project_id, overlap_key, kind, pr, merged_at, paths) VALUES \
             (1, 'same_file 51 52', 'same_file', NULL, NULL, '[\"default:config/app.yaml\"]'), \
             (1, 'main_moved 51', 'main_moved', 'acme/widgets#7', 30, \
             '[\"default:config/app.yaml\"]');\n\
             INSERT INTO overlap_items (project_id, overlap_key, item_id) VALUES \
             (1, 'same_file 51 52', 51), (1, 'same_file 51 52', 52), (1, 'main_moved 51', 51);",
            rows_at_deliveries()
        );
        let (_dir, path) = database_at(17, &rows).await;
        let store = open(&path).await.unwrap();
        let project = ProjectId(1);
        let (alice, bob) = (MemberId(11), MemberId(12));
        let mut tx = store.begin().await.unwrap();
        let grants = tx.grants(project).await.unwrap();
        assert_eq!(grants.len(), 1);
        assert_eq!(
            grants[0].decided,
            GrantDecision::Holder,
            "every grant of before was given by its holder"
        );
        let asked = AskedGrant {
            message: MessageId(93),
            ask: GrantAsk {
                asker: bob,
                item: ItemId(52),
                area: QualifiedArea::parse("default:billing").unwrap(),
            },
            holder: alice,
        };
        let nanos = |n: i64| OffsetDateTime::UNIX_EPOCH + Duration::nanoseconds(n);
        assert_eq!(
            tx.unsettled_asked_grants(project).await.unwrap(),
            [(
                AskedGrantRecord {
                    asked: asked.clone(),
                    left_to_holder: false
                },
                nanos(25)
            )],
            "every request of before is unsettled, and dibs decides it"
        );
        assert_eq!(unanswered(&mut tx, project, alice).await, [asked]);
        let ask = tx.wide_ask(project, MessageId(91)).await.unwrap().unwrap();
        assert_eq!(ask.acked_by_dibs, None);
        assert_eq!(tx.open_wide_asks(project).await.unwrap(), [ask]);
        let overlaps = tx.overlaps(project).await.unwrap();
        assert_eq!(overlaps.len(), 2);
        assert!(
            overlaps.iter().all(|overlap| overlap.order().is_none()),
            "no overlap of before has an order"
        );
        assert!(tx.grant_blocks(project).await.unwrap().is_empty());
        let block = GrantBlock {
            asker: bob,
            asker_item: ItemId(52),
            asker_claimed_at: nanos(10),
            holder: alice,
            holder_item: ItemId(51),
            holder_claimed_at: nanos(10),
            area: QualifiedArea::parse("default:billing").unwrap(),
            revoked_at: nanos(40),
        };
        tx.insert_grant_block(project, &block).await.unwrap();
        tx.commit().await.unwrap();
        let mut tx = store.begin().await.unwrap();
        assert_eq!(tx.grant_blocks(project).await.unwrap(), [block]);
        drop(tx);
        drop(store);

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .foreign_keys(true),
            )
            .await
            .unwrap();
        for (what, bad) in [
            (
                "a moved base with an order",
                "UPDATE overlaps SET order_rule = 'older', order_first = 51 \
                 WHERE kind = 'main_moved'",
            ),
            (
                "a moved base with a tie",
                "UPDATE overlaps SET order_rule = 'tied' WHERE kind = 'main_moved'",
            ),
            (
                "a rule that tells the claims apart naming no item",
                "UPDATE overlaps SET order_rule = 'older' WHERE kind = 'same_file'",
            ),
            (
                "a tie naming an item",
                "UPDATE overlaps SET order_rule = 'tied', order_first = 51 \
                 WHERE kind = 'same_file'",
            ),
            (
                "an item landing first without a rule",
                "UPDATE overlaps SET order_first = 51 WHERE kind = 'same_file'",
            ),
            (
                "a rule the order does not know",
                "UPDATE overlaps SET order_rule = 'sooner', order_first = 51 \
                 WHERE kind = 'same_file'",
            ),
            (
                "an acknowledgement by dibs without its reason",
                "UPDATE wide_asks SET dibs_acked_at = 40",
            ),
            (
                "a reason of dibs without its acknowledgement",
                "UPDATE wide_asks SET dibs_acked_because = 'idle'",
            ),
            (
                "an acknowledgement by dibs for the holder's own reason",
                "UPDATE wide_asks SET dibs_acked_at = 40, dibs_acked_because = 'holder'",
            ),
            (
                "a grant decided by nobody the column knows",
                "UPDATE grants SET decided = 'admin'",
            ),
            (
                "a settlement that is no time",
                "UPDATE grant_asks SET settled_at = 'soon'",
            ),
            (
                "a block naming a missing member",
                "UPDATE grant_blocks SET asker_id = 99",
            ),
            (
                "a block naming a missing item",
                "UPDATE grant_blocks SET holder_item = 99",
            ),
            (
                "a block whose claim time is no time",
                "UPDATE grant_blocks SET holder_claimed_at = 'soon'",
            ),
        ] {
            assert!(
                sqlx::query(sqlx::AssertSqlSafe(bad))
                    .execute(&pool)
                    .await
                    .is_err(),
                "{what}"
            );
        }
        for good in [
            "UPDATE overlaps SET order_rule = 'tied' WHERE kind = 'same_file'",
            "UPDATE overlaps SET order_rule = 'priority', order_first = 52 \
             WHERE kind = 'same_file'",
            "UPDATE wide_asks SET dibs_acked_at = 40, dibs_acked_because = 'waited'",
            "UPDATE grants SET decided = 'in_review'",
            "UPDATE grant_asks SET settled_at = 41",
        ] {
            sqlx::query(sqlx::AssertSqlSafe(good))
                .execute(&pool)
                .await
                .unwrap();
        }
        pool.close().await;

        let store = open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        let overlaps = tx.overlaps(project).await.unwrap();
        assert_eq!(
            overlaps[0].order().and_then(|order| order.first()),
            Some(ItemId(52))
        );
        assert_eq!(
            tx.wide_ask(project, MessageId(91))
                .await
                .unwrap()
                .unwrap()
                .acked_by_dibs,
            Some(GrantDecision::Waited)
        );
        assert!(tx.open_wide_asks(project).await.unwrap().is_empty());
        assert_eq!(
            tx.grants(project).await.unwrap()[0].decided,
            GrantDecision::InReview
        );
        assert!(tx.unsettled_asked_grants(project).await.unwrap().is_empty());
        drop(tx);
        drop(store);

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
        sqlx::query(
            "UPDATE overlaps SET order_rule = 'older', order_first = 53 WHERE kind = 'same_file'",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
        let store = open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        assert!(
            matches!(tx.overlaps(project).await, Err(StoreError::Corrupt { .. })),
            "an item landing first that the overlap does not name is a corrupt row"
        );
    }

    /// Requests and announcements of before, each answered in its conversation by its holder or
    /// not: bob asks alice for billing by message 93 (nobody answers in its conversation) and by
    /// message 94 (alice answers in its conversation); bob's wide claim announces itself to alice by
    /// message 91 (alice wrote in its conversation), 96 (alice wrote only in another conversation)
    /// and 97 (alice answers in its conversation).
    fn answered_and_unanswered_rows() -> String {
        format!(
            "{}\n\
             INSERT INTO threads (project_id, id, subject, item_id, author_id, to_project, created_at) \
             VALUES (1, 83, 'Billing', 52, 12, 0, 30), (1, 84, 'Rename', 52, 12, 0, 31), \
             (1, 85, 'Rename', 52, 12, 0, 32);\n\
             INSERT INTO messages (project_id, id, thread_id, author_id, label, body, importance, \
             ack_required, created_at) VALUES \
             (1, 94, 83, 12, 'web', 'May I?', 'normal', 0, 33), \
             (1, 95, 83, 11, 'web', 'Not yet.', 'normal', 0, 34), \
             (1, 96, 84, 12, 'web', 'Renaming.', 'normal', 1, 35), \
             (1, 97, 85, 12, 'web', 'Renaming.', 'normal', 1, 36), \
             (1, 98, 85, 11, 'web', 'Wait for me.', 'normal', 0, 37), \
             (1, 99, 82, NULL, 'dibs', 'Reminder.', 'normal', 0, 38);\n\
             INSERT INTO grant_asks (project_id, message_id, asker_id, item_id, repo, area, \
             holder_id) VALUES (1, 93, 12, 52, 'default', 'billing', 11), \
             (1, 94, 12, 52, 'default', 'billing', 11);\n\
             INSERT INTO wide_asks (project_id, message_id, item_id, from_id, to_id, areas) VALUES \
             (1, 96, 52, 12, 11, '[\"default:billing\"]'), \
             (1, 97, 52, 12, 11, '[\"default:billing\"]');",
            rows_at_deliveries()
        )
    }

    #[tokio::test]
    async fn server_decisions_leave_to_the_holder_what_they_answered_before_and_decide_the_rest() {
        use time::{Duration, OffsetDateTime};

        let (_dir, path) = database_at(17, &answered_and_unanswered_rows()).await;
        let store = open(&path).await.unwrap();
        let project = ProjectId(1);
        let alice = MemberId(11);
        let nanos = |n: i64| OffsetDateTime::UNIX_EPOCH + Duration::nanoseconds(n);
        let mut tx = store.begin().await.unwrap();
        let unsettled = tx.unsettled_asked_grants(project).await.unwrap();
        let decided: Vec<(MessageId, OffsetDateTime)> = unsettled
            .iter()
            .filter(|(record, _)| !record.left_to_holder)
            .map(|(record, at)| (record.asked.message, *at))
            .collect();
        assert_eq!(
            decided,
            [(MessageId(93), nanos(25))],
            "a request its holder answered is left to them; dibs' own message answers nothing"
        );
        let left: Vec<MessageId> = unsettled
            .iter()
            .filter(|(record, _)| record.left_to_holder)
            .map(|(record, _)| record.asked.message)
            .collect();
        assert_eq!(
            left,
            [MessageId(94)],
            "still unsettled, so that it is settled once it closes"
        );
        let listed: Vec<(MessageId, bool)> = tx
            .unanswered_asked_grants(project, alice)
            .await
            .unwrap()
            .into_iter()
            .map(|record| (record.asked.message, record.left_to_holder))
            .collect();
        assert_eq!(listed, [(MessageId(93), false)]);
        let still_open: Vec<MessageId> = tx
            .open_wide_asks(project)
            .await
            .unwrap()
            .into_iter()
            .map(|ask| ask.message)
            .collect();
        assert_eq!(
            still_open,
            [MessageId(91), MessageId(96), MessageId(97)],
            "every announcement of before stays open for its holder"
        );
        let unsettled = tx.open_wide_asks_with_time(project).await.unwrap();
        let judged: Vec<(MessageId, OffsetDateTime)> = unsettled
            .iter()
            .filter(|(record, _)| !record.left_to_holder)
            .map(|(record, at)| (record.ask.message, *at))
            .collect();
        assert_eq!(
            judged,
            [(MessageId(96), nanos(35))],
            "dibs judges only the one its holder did not answer in its own conversation"
        );
        let left: Vec<MessageId> = unsettled
            .iter()
            .filter(|(record, _)| record.left_to_holder)
            .map(|(record, _)| record.ask.message)
            .collect();
        assert_eq!(
            left,
            [MessageId(91), MessageId(97)],
            "still unsettled, so that each is settled once it closes"
        );
        drop(tx);
        drop(store);

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
        for (what, bad) in [
            (
                "a request neither left to its holder nor not",
                "UPDATE grant_asks SET left_to_holder = 2",
            ),
            (
                "a request with no answer to whether it is left to its holder",
                "UPDATE grant_asks SET left_to_holder = NULL",
            ),
            (
                "an announcement neither left to its holder nor not",
                "UPDATE wide_asks SET left_to_holder = 2",
            ),
            (
                "an announcement both settled and acknowledged by dibs",
                "UPDATE wide_asks SET settled_at = 40, dibs_acked_at = 40, \
                 dibs_acked_because = 'idle' WHERE message_id = 96",
            ),
            (
                "a settlement that is no time",
                "UPDATE wide_asks SET settled_at = 'soon'",
            ),
        ] {
            assert!(
                sqlx::query(sqlx::AssertSqlSafe(bad))
                    .execute(&pool)
                    .await
                    .is_err(),
                "{what}"
            );
        }
        sqlx::query("UPDATE wide_asks SET settled_at = 40 WHERE message_id = 96")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
        let store = open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        let unsettled: Vec<MessageId> = tx
            .open_wide_asks_with_time(project)
            .await
            .unwrap()
            .into_iter()
            .map(|(record, _)| record.ask.message)
            .collect();
        assert_eq!(
            unsettled,
            [MessageId(91), MessageId(97)],
            "a settled announcement is not judged; those left to their holders still wait to close"
        );
        assert_eq!(tx.open_wide_asks(project).await.unwrap().len(), 3);
    }
}
