//! The retention as the server runs it: a background task over the real store, deciding by the
//! clock it is given, that runs a pass at once, says what it did in the log, and stops when told
//! to.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use time::OffsetDateTime;
use time::macros::datetime;
use tracing::instrument::WithSubscriber;

use dibs_app::Dependencies;
use dibs_app::actor::Actor;
use dibs_app::fakes::FixedClock;
use dibs_app::ports::Store;
use dibs_app::use_cases::{
    Addressees, InitProject, InitProjectInput, NewMember, StartThread, StartThreadInput, UseCases,
};
use dibs_domain::event::EventKind;
use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::ProjectId;
use dibs_domain::member::DisplayName;
use dibs_domain::message::{Body, Importance, SessionLabel, Subject};
use dibs_domain::project::ProjectSlug;
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_domain::settings::SettingsDocument;
use dibs_server::adapters::{BroadcastNotifier, OsTokens, Sha256Hasher, TimeIds};
use dibs_server::retention::run_retention;
use dibs_server::stream::Shutdown;
use dibs_store_sqlite::SqliteStore;

use crate::support::Place;

/// A project served over a real store by a clock the test moves, with its admin signed in.
struct Served {
    store: Arc<SqliteStore>,
    clock: Arc<FixedClock>,
    start: OffsetDateTime,
    deps: Dependencies,
    app: Arc<UseCases>,
    project: ProjectId,
    alice: Actor,
}

impl Served {
    async fn new(place: &Place) -> Self {
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
        Self {
            store,
            clock,
            start,
            deps,
            app,
            project: created.project.id,
            alice,
        }
    }

    /// Alice starts a conversation with the project and writes in it once.
    async fn note_to_self(&self) {
        StartThread::new(&self.deps)
            .execute(
                &self.alice,
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
    }

    /// The kinds of the project's events, oldest first.
    async fn kinds(&self) -> Vec<String> {
        let mut tx = self.store.begin().await.unwrap();
        let events = tx.events_after(self.project, None, 50).await.unwrap();
        events
            .iter()
            .map(|event| event.kind.name().to_owned())
            .collect()
    }
}

#[tokio::test]
async fn the_retention_removes_what_its_clock_says_is_past_its_date_and_stops_when_told() {
    let place = Place::new();
    let served = Served::new(&place).await;
    let (app, alice) = (&served.app, &served.alice);
    served.note_to_self().await;
    served.clock.set(served.start + time::Duration::days(181));

    let shutdown = Shutdown::new();
    let task = tokio::spawn(run_retention(
        app.clone(),
        Duration::from_secs(3600),
        shutdown.signal(),
    ));
    let mut left = 1;
    for _ in 0..100 {
        left = app.list_threads.execute(alice, None).await.unwrap().len();
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

    let mut tx = served.store.begin().await.unwrap();
    let events = tx.events_after(served.project, None, 50).await.unwrap();
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
    served.store.close().await;
}

/// What the retention logs, one JSON line per record, kept in memory.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Captured {
    /// Each record's level and message, in the order logged.
    fn records(&self) -> Vec<(String, String)> {
        let text = String::from_utf8(self.0.lock().unwrap().clone()).unwrap();
        text.lines()
            .map(|line| {
                let record: serde_json::Value = serde_json::from_str(line).unwrap();
                let field = |value: &serde_json::Value| value.as_str().unwrap().to_owned();
                (field(&record["level"]), field(&record["fields"]["message"]))
            })
            .collect()
    }
}

#[tokio::test]
async fn a_project_whose_settings_do_not_parse_is_retained_like_any_other_with_nothing_to_warn_of()
{
    let place = Place::new();
    let served = Served::new(&place).await;
    served.note_to_self().await;
    let unknown: SettingsDocument =
        serde_json::from_str(r#"{"hot": [{"paths": ["**/*.lock"], "kind": "rebuild-on-merge"}]}"#)
            .unwrap();
    let mut tx = served.store.begin().await.unwrap();
    tx.put_settings(served.project, &unknown).await.unwrap();
    tx.commit().await.unwrap();
    served.clock.set(served.start + time::Duration::days(181));

    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_writer(move || writer.clone())
        .finish();
    let shutdown = Shutdown::new();
    let task = tokio::spawn(
        run_retention(
            served.app.clone(),
            Duration::from_secs(3600),
            shutdown.signal(),
        )
        .with_subscriber(subscriber),
    );
    for _ in 0..100 {
        if served.kinds().await == ["pruned"] {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    shutdown.stop();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("the retention stops when told")
        .unwrap();

    assert_eq!(
        served.kinds().await,
        ["pruned"],
        "the old events went all the same"
    );
    let records = captured.records();
    let said = |level: &str| -> Vec<&str> {
        records
            .iter()
            .filter(|(at, _)| at == level)
            .map(|(_, message)| message.as_str())
            .collect()
    };
    assert_eq!(said("ERROR"), Vec::<&str>::new(), "nothing failed");
    assert_eq!(
        said("WARN"),
        Vec::<&str>::new(),
        "the retention reads no settings"
    );
    served.store.close().await;
}
