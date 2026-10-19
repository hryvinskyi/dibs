//! The operator's work on the store's file: a copy taken while it is open holds what was
//! committed and opens as a store of its own, it is written while a writer holds the write lock,
//! a copy never overwrites a file, and a checkpoint empties the write-ahead log.

#[cfg(test)]
mod maintenance {
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;
    use time::OffsetDateTime;

    use dibs_app::ports::Store;
    use dibs_app::records::ProjectRecord;
    use dibs_domain::ids::ProjectId;
    use dibs_domain::project::ProjectSlug;
    use dibs_store_sqlite::{MaintenanceError, SqliteStore};

    fn dir() -> (TempDir, PathBuf) {
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

    async fn insert(store: &SqliteStore, record: &ProjectRecord) {
        let mut tx = store.begin().await.unwrap();
        tx.insert_project(record).await.unwrap();
        tx.commit().await.unwrap();
    }

    fn wal(path: &Path) -> PathBuf {
        let mut name = path.as_os_str().to_owned();
        name.push("-wal");
        PathBuf::from(name)
    }

    #[tokio::test]
    async fn a_copy_holds_what_was_committed_and_opens_on_its_own() {
        let (dir, path) = dir();
        let store = SqliteStore::open(&path).await.unwrap();
        insert(&store, &project(1, "widgets")).await;
        let copy = dir.path().join("copy.db");
        store.vacuum_into(&copy).await.unwrap();
        insert(&store, &project(2, "gadgets")).await;

        let copied = SqliteStore::open(&copy).await.unwrap();
        let mut tx = copied.begin().await.unwrap();
        assert_eq!(
            tx.project(ProjectId(1)).await.unwrap(),
            Some(project(1, "widgets"))
        );
        assert_eq!(tx.project(ProjectId(2)).await.unwrap(), None);
        drop(tx);
        copied.close().await;
        store.close().await;
    }

    #[tokio::test]
    async fn a_copy_takes_no_write_lock_while_a_writer_holds_one() {
        let (dir, path) = dir();
        let store = SqliteStore::open(&path).await.unwrap();
        insert(&store, &project(1, "widgets")).await;
        let mut writer = store.begin().await.unwrap();
        writer.insert_project(&project(2, "gadgets")).await.unwrap();

        let copy = dir.path().join("copy.db");
        let started = std::time::Instant::now();
        store.vacuum_into(&copy).await.unwrap();
        assert!(
            started.elapsed() < dibs_store_sqlite::BUSY_TIMEOUT,
            "the copy did not wait for the writer"
        );
        writer.commit().await.unwrap();

        let copied = SqliteStore::open(&copy).await.unwrap();
        let mut tx = copied.begin().await.unwrap();
        assert!(tx.project(ProjectId(1)).await.unwrap().is_some());
        assert_eq!(
            tx.project(ProjectId(2)).await.unwrap(),
            None,
            "what was not committed when the copy began is not in it"
        );
        drop(tx);
        copied.close().await;
        store.close().await;
    }

    #[tokio::test]
    async fn a_copy_never_replaces_a_file() {
        let (dir, path) = dir();
        let store = SqliteStore::open(&path).await.unwrap();
        let copy = dir.path().join("copy.db");
        std::fs::write(&copy, b"kept").unwrap();
        let error = store.vacuum_into(&copy).await.unwrap_err();
        assert!(matches!(error, MaintenanceError::Copy(_)), "{error:?}");
        assert_eq!(std::fs::read(&copy).unwrap(), b"kept");
        let missing = dir.path().join("absent").join("copy.db");
        let error = store.vacuum_into(&missing).await.unwrap_err();
        assert!(matches!(error, MaintenanceError::Copy(_)), "{error:?}");
        store.close().await;
    }

    #[tokio::test]
    async fn a_checkpoint_moves_the_log_into_the_database_and_empties_it() {
        let (_dir, path) = dir();
        let store = SqliteStore::open(&path).await.unwrap();
        insert(&store, &project(1, "widgets")).await;
        assert!(std::fs::metadata(wal(&path)).unwrap().len() > 0);
        let checkpoint = store.checkpoint().await.unwrap();
        assert!(checkpoint.complete, "{checkpoint:?}");
        assert_eq!(checkpoint.frames, checkpoint.moved);
        assert_eq!(std::fs::metadata(wal(&path)).unwrap().len(), 0);
        store.close().await;
        assert!(
            std::fs::metadata(wal(&path)).map_or(true, |meta| meta.len() == 0),
            "the log is empty or gone once the store is closed"
        );
    }
}
