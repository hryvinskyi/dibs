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
        assert_eq!(version, 4);
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
}
