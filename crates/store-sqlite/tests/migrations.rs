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
    use dibs_domain::ids::{ItemId, MemberId, MessageId, ProjectId, ThreadId};
    use dibs_domain::item::{ExternalRef, ItemStatus, Priority};
    use dibs_domain::message::{Ack, Importance};
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
        assert_eq!(version, 3);
    }
}
