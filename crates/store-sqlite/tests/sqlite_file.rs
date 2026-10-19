//! What the SQLite store does with its file: the schema it creates and keeps across opens, the
//! journal mode and foreign keys it runs with, connections handed back clean after a transaction
//! that never committed, and rows it cannot read reported as corrupt.

#[cfg(test)]
mod sqlite_file {
    use std::path::{Path, PathBuf};

    use sqlx::sqlite::SqliteConnectOptions;
    use sqlx::{Connection, SqliteConnection};
    use tempfile::TempDir;
    use time::OffsetDateTime;

    use dibs_app::ports::{Store, StoreError};
    use dibs_app::records::{MemberRecord, ProjectRecord};
    use dibs_domain::handle::Handle;
    use dibs_domain::ids::{MemberId, ProjectId};
    use dibs_domain::member::{DisplayName, Role};
    use dibs_domain::project::ProjectSlug;
    use dibs_store_sqlite::{OpenError, POOL_SIZE, SqliteStore};

    fn file() -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dibs.db");
        (dir, path)
    }

    fn project(id: u64, slug: &str) -> ProjectRecord {
        ProjectRecord {
            id: ProjectId(id),
            slug: ProjectSlug::parse(slug).unwrap(),
            created_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn member(id: u64, handle: &str) -> MemberRecord {
        MemberRecord {
            id: MemberId(id),
            handle: Handle::parse(handle).unwrap(),
            display_name: DisplayName::parse(handle).unwrap(),
            external_login: None,
            role: Role::Member,
            created_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    /// A connection of its own to the file, beside the store's pool.
    async fn raw(path: &Path) -> SqliteConnection {
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(path))
            .await
            .unwrap()
    }

    async fn scalar(path: &Path, sql: &'static str) -> String {
        let mut connection = raw(path).await;
        let row: (String,) = sqlx::query_as(sql)
            .fetch_one(&mut connection)
            .await
            .unwrap();
        row.0
    }

    #[tokio::test]
    async fn the_schema_is_created_in_an_empty_file_and_kept_on_the_next_open() {
        let (_dir, path) = file();
        let store = SqliteStore::open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        tx.insert_project(&project(1, "widgets")).await.unwrap();
        tx.commit().await.unwrap();
        store.close().await;

        let store = SqliteStore::open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        assert_eq!(
            tx.project(ProjectId(1)).await.unwrap(),
            Some(project(1, "widgets"))
        );
        tx.insert_project(&project(2, "gadgets")).await.unwrap();
        tx.commit().await.unwrap();
        store.close().await;

        let applied = scalar(
            &path,
            "SELECT group_concat(version || ':' || success) FROM _sqlx_migrations",
        )
        .await;
        assert_eq!(applied, "1:1,2:1,3:1");
        assert_eq!(scalar(&path, "PRAGMA journal_mode").await, "wal");
    }

    #[tokio::test]
    async fn the_messages_that_ask_for_an_ack_are_read_through_their_index() {
        let (_dir, path) = file();
        SqliteStore::open(&path).await.unwrap().close().await;
        let mut connection = raw(&path).await;
        let steps: Vec<(i64, i64, i64, String)> = sqlx::query_as(
            "EXPLAIN QUERY PLAN SELECT id FROM messages m \
             WHERE m.project_id = 1 AND m.ack_required = 1 AND m.author_id <> 2 \
             ORDER BY m.created_at DESC, m.id DESC",
        )
        .fetch_all(&mut connection)
        .await
        .unwrap();
        let plan = steps
            .into_iter()
            .map(|(_, _, _, detail)| detail)
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(plan.contains("messages_asking_ack"), "{plan}");
    }

    #[tokio::test]
    async fn what_an_item_unblocks_is_read_through_its_index() {
        let (_dir, path) = file();
        SqliteStore::open(&path).await.unwrap().close().await;
        let mut connection = raw(&path).await;
        let steps: Vec<(i64, i64, i64, String)> = sqlx::query_as(
            "EXPLAIN QUERY PLAN SELECT item_id, source FROM dependencies \
             WHERE project_id = 1 AND blocked_by_id = 7",
        )
        .fetch_all(&mut connection)
        .await
        .unwrap();
        let plan = steps
            .into_iter()
            .map(|(_, _, _, detail)| detail)
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(plan.contains("dependencies_by_blocker"), "{plan}");
    }

    #[tokio::test]
    async fn completions_since_a_time_are_read_through_their_index() {
        let (_dir, path) = file();
        SqliteStore::open(&path).await.unwrap().close().await;
        let mut connection = raw(&path).await;
        let steps: Vec<(i64, i64, i64, String)> = sqlx::query_as(
            "EXPLAIN QUERY PLAN SELECT id FROM events \
             WHERE project_id = 1 AND kind = 'completed' AND at >= 0 \
             ORDER BY at DESC, id DESC LIMIT 10",
        )
        .fetch_all(&mut connection)
        .await
        .unwrap();
        let plan = steps
            .into_iter()
            .map(|(_, _, _, detail)| detail)
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(plan.contains("events_by_kind_and_time"), "{plan}");
        assert!(!plan.contains("SCAN events"), "{plan}");
    }

    #[tokio::test]
    async fn two_stores_open_one_file_in_turn_and_share_its_rows() {
        let (_dir, path) = file();
        let first = SqliteStore::open(&path).await.unwrap();
        let second = SqliteStore::open(&path).await.unwrap();
        let mut tx = first.begin().await.unwrap();
        tx.insert_project(&project(1, "widgets")).await.unwrap();
        tx.commit().await.unwrap();
        let mut tx = second.begin().await.unwrap();
        assert!(tx.project(ProjectId(1)).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn a_file_that_is_not_a_database_is_refused() {
        let (_dir, path) = file();
        std::fs::write(
            &path,
            b"these bytes are not a database, however long they go on",
        )
        .unwrap();
        assert!(matches!(
            SqliteStore::open(&path).await,
            Err(OpenError::Connect(_) | OpenError::Migrate(_))
        ));
    }

    #[tokio::test]
    async fn a_row_pointing_at_a_record_that_does_not_exist_fails_the_commit() {
        let (_dir, path) = file();
        let store = SqliteStore::open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        tx.insert_member(ProjectId(1), &member(11, "alice"))
            .await
            .unwrap();
        assert!(matches!(tx.commit().await, Err(StoreError::Backend(_))));

        let mut tx = store.begin().await.unwrap();
        assert_eq!(tx.members(ProjectId(1)).await.unwrap(), Vec::new());
        tx.insert_project(&project(1, "widgets")).await.unwrap();
        tx.insert_member(ProjectId(1), &member(11, "alice"))
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    #[tokio::test]
    async fn every_connection_comes_back_clean_after_a_dropped_transaction() {
        let (_dir, path) = file();
        let store = SqliteStore::open(&path).await.unwrap();
        for n in 1..=u64::from(POOL_SIZE) * 2 {
            let mut tx = store.begin().await.unwrap();
            tx.insert_project(&project(n, &format!("dropped-{n}")))
                .await
                .unwrap();
            drop(tx);
        }
        let mut tx = store.begin().await.unwrap();
        for n in 1..=u64::from(POOL_SIZE) * 2 {
            assert_eq!(tx.project(ProjectId(n)).await.unwrap(), None);
        }
        tx.insert_project(&project(100, "kept")).await.unwrap();
        tx.commit().await.unwrap();
        let mut tx = store.begin().await.unwrap();
        assert!(tx.project(ProjectId(100)).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn an_id_above_the_largest_stored_integer_is_refused() {
        let (_dir, path) = file();
        let store = SqliteStore::open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        assert!(matches!(
            tx.insert_project(&project(u64::MAX, "widgets")).await,
            Err(StoreError::Backend(_))
        ));
        tx.insert_project(&project(1, "widgets")).await.unwrap();
        tx.commit().await.unwrap();
    }

    #[tokio::test]
    async fn a_row_that_does_not_parse_is_reported_as_corrupt() {
        let (_dir, path) = file();
        let store = SqliteStore::open(&path).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        tx.insert_project(&project(1, "widgets")).await.unwrap();
        tx.insert_member(ProjectId(1), &member(11, "alice"))
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let mut connection = raw(&path).await;
        sqlx::query("UPDATE members SET role = 'owner' WHERE id = 11")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("UPDATE projects SET slug = 'Not A Slug' WHERE id = 1")
            .execute(&mut connection)
            .await
            .unwrap();
        connection.close().await.unwrap();

        let mut tx = store.begin().await.unwrap();
        assert!(matches!(
            tx.member(ProjectId(1), MemberId(11)).await,
            Err(StoreError::Corrupt { what }) if what.starts_with("members.role")
        ));
        assert!(matches!(
            tx.project(ProjectId(1)).await,
            Err(StoreError::Corrupt { what }) if what.starts_with("projects.slug")
        ));
    }
}
