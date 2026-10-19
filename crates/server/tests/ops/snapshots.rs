use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use time::macros::datetime;

use dibs_app::fakes::FixedClock;
use dibs_app::ports::{Clock, Store};
use dibs_app::records::ProjectRecord;
use dibs_domain::ids::ProjectId;
use dibs_domain::project::ProjectSlug;
use dibs_server::adapters::SystemClock;
use dibs_server::config::SnapshotConfig;
use dibs_server::snapshot::{SnapshotError, SnapshotName, Snapshots, run_schedule};
use dibs_server::stream::Shutdown;
use dibs_store_sqlite::SqliteStore;

use crate::support::{Place, Server, init, run};

fn project(id: u64, slug: &str) -> ProjectRecord {
    ProjectRecord {
        id: ProjectId(id),
        slug: ProjectSlug::parse(slug).unwrap(),
        created_at: time::OffsetDateTime::UNIX_EPOCH,
    }
}

async fn insert(store: &SqliteStore, record: &ProjectRecord) {
    let mut tx = store.begin().await.unwrap();
    tx.insert_project(record).await.unwrap();
    tx.commit().await.unwrap();
}

fn settings(dir: &Path, keep: usize) -> SnapshotConfig {
    SnapshotConfig {
        dir: dir.to_owned(),
        every: Duration::from_secs(60),
        keep: NonZeroUsize::new(keep).unwrap(),
    }
}

#[tokio::test]
async fn a_snapshot_is_a_database_that_opens_and_holds_the_data() {
    let place = Place::new();
    let store = Arc::new(SqliteStore::open(&place.database()).await.unwrap());
    insert(&store, &project(1, "widgets")).await;
    let clock = Arc::new(FixedClock::new(datetime!(2026-03-02 09:00:00.250 UTC)));
    let snapshots = Snapshots::new(store.clone(), clock, &settings(&place.snapshots(), 3));

    let taken = snapshots.take().await.unwrap();
    insert(&store, &project(2, "gadgets")).await;
    assert_eq!(
        taken.path,
        place.snapshots().join("dibs-20260302T090000.250Z.db")
    );
    assert_eq!(taken.bytes, std::fs::metadata(&taken.path).unwrap().len());
    assert!(taken.bytes > 0);
    assert_eq!(taken.removed, 0);
    assert_eq!(place.snapshot_files(), ["dibs-20260302T090000.250Z.db"]);

    let copy = SqliteStore::open(&taken.path).await.unwrap();
    let mut tx = copy.begin().await.unwrap();
    assert_eq!(
        tx.project(ProjectId(1)).await.unwrap(),
        Some(project(1, "widgets"))
    );
    assert_eq!(tx.project(ProjectId(2)).await.unwrap(), None);
    drop(tx);
    copy.close().await;
    store.close().await;
}

#[tokio::test]
async fn retention_deletes_the_oldest_and_leaves_other_files_alone() {
    let place = Place::new();
    let store = Arc::new(SqliteStore::open(&place.database()).await.unwrap());
    std::fs::write(place.snapshots().join("notes.txt"), b"kept").unwrap();
    std::fs::write(place.snapshots().join("dibs.db"), b"kept").unwrap();
    let clock = Arc::new(FixedClock::new(datetime!(2026-03-02 00:00 UTC)));
    let snapshots = Snapshots::new(
        store.clone(),
        clock.clone(),
        &settings(&place.snapshots(), 2),
    );
    let mut removed = Vec::new();
    for _ in 0..4 {
        removed.push(snapshots.take().await.unwrap().removed);
        clock.advance(time::Duration::hours(6));
    }
    assert_eq!(removed, [0, 0, 1, 1]);
    assert_eq!(
        place.snapshot_files(),
        [
            "dibs-20260302T120000.000Z.db",
            "dibs-20260302T180000.000Z.db",
            "dibs.db",
            "notes.txt",
        ]
    );
    store.close().await;
}

#[tokio::test]
async fn a_snapshot_of_a_taken_moment_is_refused_and_replaces_nothing() {
    let place = Place::new();
    let store = Arc::new(SqliteStore::open(&place.database()).await.unwrap());
    let clock = Arc::new(FixedClock::new(datetime!(2026-03-02 09:00 UTC)));
    let snapshots = Snapshots::new(store.clone(), clock, &settings(&place.snapshots(), 3));
    let first = snapshots.take().await.unwrap();
    let bytes = std::fs::read(&first.path).unwrap();
    let error = snapshots.take().await.unwrap_err();
    assert!(matches!(error, SnapshotError::Exists(_)), "{error:?}");
    assert_eq!(std::fs::read(&first.path).unwrap(), bytes);
    assert_eq!(place.snapshot_files().len(), 1);
    store.close().await;
}

#[tokio::test]
async fn a_failed_snapshot_leaves_nothing_and_the_next_tick_tries_again() {
    let place = Place::new();
    let store = Arc::new(SqliteStore::open(&place.database()).await.unwrap());
    insert(&store, &project(1, "widgets")).await;
    let missing = place.path("later");
    let snapshots = Arc::new(Snapshots::new(
        store.clone(),
        Arc::new(SystemClock),
        &settings(&missing, 3),
    ));
    let error = snapshots.take().await.unwrap_err();
    assert!(matches!(error, SnapshotError::Copy { .. }), "{error:?}");
    assert!(
        error
            .to_string()
            .contains("check that the directory exists"),
        "{error}"
    );

    let shutdown = Shutdown::new();
    let schedule = tokio::spawn(run_schedule(
        snapshots.clone(),
        Duration::from_millis(300),
        shutdown.signal(),
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !missing.exists(),
        "the first tick failed and the server lives on"
    );
    std::fs::create_dir(&missing).unwrap();
    let mut written = Vec::new();
    for _ in 0..50 {
        written = snapshots.present().unwrap();
        if !written.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!written.is_empty(), "the next tick wrote a snapshot");
    shutdown.stop();
    tokio::time::timeout(Duration::from_secs(5), schedule)
        .await
        .unwrap()
        .unwrap();
    let leftovers: Vec<_> = std::fs::read_dir(&missing)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().ends_with(".partial"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    store.close().await;
}

#[tokio::test]
async fn the_first_scheduled_snapshot_waits_for_one_interval_after_the_newest() {
    let place = Place::new();
    let store = Arc::new(SqliteStore::open(&place.database()).await.unwrap());
    let clock = Arc::new(FixedClock::new(datetime!(2026-03-02 09:00 UTC)));
    let snapshots = Snapshots::new(
        store.clone(),
        clock.clone(),
        &settings(&place.snapshots(), 3),
    );
    let every = Duration::from_secs(6 * 60 * 60);
    assert_eq!(snapshots.first_wait(every), Duration::ZERO, "none yet");
    snapshots.take().await.unwrap();
    clock.advance(time::Duration::hours(2));
    assert_eq!(
        snapshots.first_wait(every),
        Duration::from_secs(4 * 60 * 60)
    );
    clock.advance(time::Duration::hours(5));
    assert_eq!(snapshots.first_wait(every), Duration::ZERO, "overdue");
    clock.set(datetime!(2026-03-01 09:00 UTC));
    assert_eq!(
        snapshots.first_wait(every),
        every,
        "the newest is from the future"
    );
    assert_eq!(
        SnapshotName::taken(clock.now()).file_name(),
        "dibs-20260301T090000.000Z.db"
    );
    store.close().await;
}

#[test]
fn the_admin_command_writes_a_snapshot_while_the_server_runs() {
    let place = Place::new();
    init(&place.database());
    let _server = Server::spawn(&place.database(), &[]);
    let ran = run(
        &place.database(),
        &["admin", "snapshot"],
        &[("DIBS_SNAPSHOT_DIR", &place.snapshots())],
    );
    assert_eq!(ran.code, Some(0), "{ran:?}");
    assert!(ran.stdout.starts_with("Wrote "), "{ran:?}");
    assert!(ran.stdout.contains("bytes in"), "{ran:?}");
    let files = place.snapshot_files();
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(SnapshotName::parse(&files[0]).is_some(), "{files:?}");
    assert!(ran.stderr.contains("snapshot written"), "{ran:?}");
}

#[test]
fn a_server_with_a_snapshot_directory_takes_one_at_once_when_it_holds_none() {
    let place = Place::new();
    init(&place.database());
    let mut server = Server::spawn(
        &place.database(),
        &[("DIBS_SNAPSHOT_DIR", &place.snapshots())],
    );
    let written = server.log("snapshot written", Duration::from_secs(30));
    assert!(
        written["fields"]["bytes"].as_u64().unwrap() > 0,
        "{written}"
    );
    assert!(written["fields"]["duration_ms"].is_u64(), "{written}");
    let files = place.snapshot_files();
    assert_eq!(files.len(), 1, "{files:?}");
}
