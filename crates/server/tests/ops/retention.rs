//! The retention as the server runs it: a background task over the real store, deciding by the
//! clock it is given, that runs a pass at once and stops when told to.

use std::sync::Arc;
use std::time::Duration;

use time::macros::datetime;

use dibs_app::Dependencies;
use dibs_app::fakes::FixedClock;
use dibs_app::ports::Store;
use dibs_app::use_cases::{
    Addressees, InitProject, InitProjectInput, NewMember, StartThread, StartThreadInput, UseCases,
};
use dibs_domain::event::EventKind;
use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::member::DisplayName;
use dibs_domain::message::{Body, Importance, SessionLabel, Subject};
use dibs_domain::project::ProjectSlug;
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_server::adapters::{BroadcastNotifier, OsTokens, Sha256Hasher, TimeIds};
use dibs_server::retention::run_retention;
use dibs_server::stream::Shutdown;
use dibs_store_sqlite::SqliteStore;

use crate::support::Place;

#[tokio::test]
async fn the_retention_removes_what_its_clock_says_is_past_its_date_and_stops_when_told() {
    let place = Place::new();
    let store = Arc::new(SqliteStore::open(&place.database()).await.unwrap());
    let start = datetime!(2026-03-02 09:00 UTC);
    let clock = Arc::new(FixedClock::new(start));
    let deps = Dependencies {
        store: store.clone(),
        clock: clock.clone(),
        ids: Arc::new(TimeIds::default()),
        tokens: Arc::new(OsTokens),
        hasher: Arc::new(Sha256Hasher),
        notifier: Arc::new(BroadcastNotifier::new(8)),
        hot_rules: Arc::new(HotRuleRegistry::standard()),
        ref_schemes: Arc::new(RefSchemeRegistry::standard()),
    };
    let created = InitProject::new(&deps)
        .execute(InitProjectInput {
            slug: ProjectSlug::parse("widgets").unwrap(),
            admin: NewMember {
                handle: Handle::parse("alice").unwrap(),
                display_name: DisplayName::parse("Alice").unwrap(),
                external_login: None,
            },
        })
        .await
        .unwrap();
    let app = Arc::new(UseCases::new(&deps));
    let alice = app
        .authenticate
        .execute(created.token.expose())
        .await
        .unwrap();
    StartThread::new(&deps)
        .execute(
            &alice,
            StartThreadInput {
                subject: Subject::parse("Notes to self").unwrap(),
                to: Addressees::Project,
                item: None,
                label: SessionLabel::parse("web").unwrap(),
                body: Body::parse("Remember the rate table.").unwrap(),
                importance: Importance::Normal,
                ack_required: false,
            },
        )
        .await
        .unwrap();
    clock.set(start + time::Duration::days(181));

    let shutdown = Shutdown::new();
    let task = tokio::spawn(run_retention(
        app.clone(),
        Duration::from_secs(3600),
        shutdown.signal(),
    ));
    let mut left = 1;
    for _ in 0..100 {
        left = app.list_threads.execute(&alice, None).await.unwrap().len();
        if left == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        left, 0,
        "the first pass ran at once and removed the quiet thread"
    );
    shutdown.stop();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("the retention stops when told")
        .unwrap();

    let mut tx = store.begin().await.unwrap();
    let events = tx.events_after(created.project.id, None, 50).await.unwrap();
    let kinds: Vec<&str> = events.iter().map(|event| event.kind.name()).collect();
    assert_eq!(
        kinds,
        ["pruned"],
        "every event of 181 days ago went, and one says so"
    );
    let EventKind::Pruned(pruned) = &events[0].kind else {
        panic!("not a pruned event: {kinds:?}");
    };
    assert_eq!((pruned.counts.threads, pruned.counts.messages), (1, 1));
    drop(tx);
    store.close().await;
}
