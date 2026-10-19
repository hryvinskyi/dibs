use std::path::Path;

use dibs_app::ports::Store;
use dibs_domain::project::ProjectSlug;
use dibs_server::hold::{DatabaseHold, HoldError};
use dibs_server::restore::{RestoreError, restore};
use dibs_store_sqlite::SqliteStore;

use crate::support::{Place, Server, body_of, init, request, run};

/// Takes a snapshot through the admin command and answers its path.
fn snapshot(place: &Place) -> std::path::PathBuf {
    let ran = run(
        &place.database(),
        &["admin", "snapshot"],
        &[("DIBS_SNAPSHOT_DIR", &place.snapshots())],
    );
    assert_eq!(ran.code, Some(0), "{ran:?}");
    let files = place.snapshot_files();
    assert_eq!(files.len(), 1, "{files:?}");
    place.snapshots().join(&files[0])
}

async fn has_project(database: &Path, slug: &str) -> bool {
    let store = SqliteStore::open(database).await.unwrap();
    let mut tx = store.begin().await.unwrap();
    let found = tx
        .project_by_slug(&ProjectSlug::parse(slug).unwrap())
        .await
        .unwrap()
        .is_some();
    drop(tx);
    store.close().await;
    found
}

#[test]
fn a_snapshot_restores_into_a_new_database_the_server_serves() {
    let place = Place::new();
    let token = init(&place.database());
    let snapshot = snapshot(&place);
    let before = std::fs::read(&snapshot).unwrap();
    let target = place.path("restored.db");

    let ran = run(
        &place.database(),
        &[
            "restore",
            snapshot.to_str().unwrap(),
            "--to",
            target.to_str().unwrap(),
        ],
        &[],
    );
    assert_eq!(ran.code, Some(0), "{ran:?}");
    assert!(ran.stdout.starts_with("Restored "), "{ran:?}");
    assert_eq!(
        std::fs::read(&snapshot).unwrap(),
        before,
        "the snapshot is not written to"
    );
    let leftovers: Vec<String> = std::fs::read_dir(place.path(""))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.contains("restoring"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    let server = Server::spawn(&target, &[]);
    let (status, answer) = request(server.address, "GET", "/v1/me", &token, None);
    assert_eq!(status, 200, "{answer}");
    assert_eq!(body_of(&answer)["project"]["slug"], "widgets");
}

#[tokio::test]
async fn the_restored_file_opens_and_migrates() {
    let place = Place::new();
    init(&place.database());
    let snapshot = snapshot(&place);
    let target = place.path("restored.db");
    let restored = restore(&snapshot, &target).await.unwrap();
    assert_eq!(restored.database, target);
    assert_eq!(restored.bytes, std::fs::metadata(&target).unwrap().len());
    assert!(has_project(&target, "widgets").await);
    let migrated = run(&target, &["migrate"], &[]);
    assert_eq!(migrated.code, Some(0), "{migrated:?}");
}

#[tokio::test]
async fn a_snapshot_of_an_older_schema_is_brought_up_to_date_by_the_restore() {
    let place = Place::new();
    let empty = place.path("before-any-migration.db");
    std::fs::write(&empty, b"").unwrap();
    let target = place.path("restored.db");
    restore(&empty, &target).await.unwrap();
    let store = SqliteStore::open(&target).await.unwrap();
    let mut tx = store.begin().await.unwrap();
    let record = dibs_app::records::ProjectRecord {
        id: dibs_domain::ids::ProjectId(1),
        slug: ProjectSlug::parse("widgets").unwrap(),
        created_at: time::OffsetDateTime::UNIX_EPOCH,
    };
    tx.insert_project(&record).await.unwrap();
    tx.commit().await.unwrap();
    store.close().await;
    assert!(has_project(&target, "widgets").await);
    assert_eq!(
        std::fs::read(&empty).unwrap(),
        b"",
        "the snapshot is not written to"
    );
}

#[tokio::test]
async fn a_restore_refuses_a_target_that_exists_or_left_a_journal() {
    let place = Place::new();
    init(&place.database());
    let snapshot = snapshot(&place);

    let error = restore(&snapshot, &place.database()).await.unwrap_err();
    assert!(matches!(error, RestoreError::TargetExists(_)), "{error:?}");
    assert!(has_project(&place.database(), "widgets").await);

    let target = place.path("restored.db");
    let mut wal = target.as_os_str().to_owned();
    wal.push("-wal");
    std::fs::write(&wal, b"stale").unwrap();
    let error = restore(&snapshot, &target).await.unwrap_err();
    assert!(
        matches!(&error, RestoreError::TargetExists(path) if path.as_os_str() == wal),
        "{error:?}"
    );
    assert!(!target.exists());

    let ran = run(
        &place.database(),
        &[
            "restore",
            snapshot.to_str().unwrap(),
            "--to",
            place.database().to_str().unwrap(),
        ],
        &[],
    );
    assert_eq!(ran.code, Some(1), "{ran:?}");
    assert!(
        ran.stderr
            .contains("never replaces one, so move it away first"),
        "{ran:?}"
    );
}

#[test]
fn a_restore_refuses_while_a_server_holds_the_target() {
    let place = Place::new();
    init(&place.database());
    let snapshot = snapshot(&place);
    let target = place.path("restored.db");
    let server = Server::spawn(&target, &[]);
    assert!(target.exists(), "the server created its database");
    std::fs::remove_file(&target).unwrap();

    let ran = run(
        &place.database(),
        &[
            "restore",
            snapshot.to_str().unwrap(),
            "--to",
            target.to_str().unwrap(),
        ],
        &[],
    );
    assert_eq!(ran.code, Some(1), "{ran:?}");
    assert!(
        ran.stderr
            .contains("a running server or another restore holds"),
        "{ran:?}"
    );
    assert!(!target.exists());
    drop(server);

    let ran = run(
        &place.database(),
        &[
            "restore",
            snapshot.to_str().unwrap(),
            "--to",
            target.to_str().unwrap(),
        ],
        &[],
    );
    assert_eq!(ran.code, Some(1), "{ran:?}");
    assert!(
        ran.stderr.contains("-wal exists") || ran.stderr.contains("-shm exists"),
        "a killed server leaves its journal, which a restore never builds on: {ran:?}"
    );
}

#[tokio::test]
async fn a_restore_refuses_a_missing_snapshot_and_a_file_that_is_no_database() {
    let place = Place::new();
    let target = place.path("restored.db");
    let error = restore(&place.path("absent.db"), &target)
        .await
        .unwrap_err();
    assert!(matches!(error, RestoreError::NoSnapshot(_)), "{error:?}");
    let junk = place.path("junk.db");
    std::fs::write(&junk, b"this is no database, only some text").unwrap();
    let error = restore(&junk, &target).await.unwrap_err();
    assert!(matches!(error, RestoreError::NotADatabase(..)), "{error:?}");
    assert!(!target.exists());
    let leftovers = std::fs::read_dir(place.path(""))
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("restoring")
        })
        .count();
    assert_eq!(leftovers, 0);
}

#[test]
fn one_hold_at_a_time_and_a_released_hold_can_be_taken_again() {
    let place = Place::new();
    let database = place.database();
    let first = DatabaseHold::take(&database).unwrap();
    assert_eq!(first.lock(), DatabaseHold::lock_path(&database));
    let refused = DatabaseHold::take(&database).unwrap_err();
    assert!(matches!(&refused, HoldError::Held { database: held } if *held == database));
    drop(first);
    let again = DatabaseHold::take(&database).unwrap();
    assert!(again.lock().exists());
    assert!(!database.exists(), "taking the hold creates no database");
    let elsewhere = place.path("absent").join("dibs.db");
    assert!(matches!(
        DatabaseHold::take(&elsewhere),
        Err(HoldError::Io { .. })
    ));
}
