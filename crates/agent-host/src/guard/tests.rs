use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use serde_json::{Value, json};
use time::{Duration, OffsetDateTime};

use dibs_domain::area::QualifiedArea;
use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::ItemId;
use dibs_domain::paths::RelPath;
use dibs_domain::repo::RepoId;
use dibs_domain::settings::{ProjectSettings, SettingsDocument};

use super::*;
use crate::adapter::{HostAdapter, HostId, ParseError};
use crate::neutral::Neutral;
use crate::ports::{CoordinationGateway, HoldLine, InboxView, LocalCache, StatusView, Upkeep};

const START: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

fn at(seconds: i64) -> OffsetDateTime {
    START + Duration::days(400) + Duration::seconds(seconds)
}

fn handle(text: &str) -> Handle {
    Handle::parse(text).unwrap()
}

fn area(text: &str) -> QualifiedArea {
    QualifiedArea::parse(text).unwrap()
}

fn settings(policy: &str) -> ProjectSettings {
    let document: SettingsDocument = serde_json::from_value(json!({
        "heartbeat_every": "2m",
        "edit_policy": policy,
        "repos": [{
            "id": "api",
            "remote": "https://example.com/acme/api.git",
            "areas": [{"name": "billing", "paths": ["src/billing/**"]}],
            "hot": [{"paths": ["src/billing/rates.toml"], "kind": "warn-shared"}]
        }]
    }))
    .unwrap();
    ProjectSettings::parse(&document, &HotRuleRegistry::standard()).unwrap()
}

/// What the fakes remember and answer, shared by every port.
struct State {
    now: OffsetDateTime,
    checkout: bool,
    open_error: Option<PortError>,
    repo: Result<RepoId, PortError>,
    cached_settings: Option<Cached<KnownSettings>>,
    unreadable_settings: bool,
    status: Option<Cached<StatusView>>,
    inbox: Option<Cached<InboxView>>,
    noted: Vec<Cached<QualifiedArea>>,
    last_heartbeat: Option<OffsetDateTime>,
    unreachable: Option<OffsetDateTime>,
    last_warning: Option<OffsetDateTime>,
    last_repo_warning: Option<OffsetDateTime>,
    server_settings: Result<KnownSettings, PortError>,
    server_status: Result<StatusView, PortError>,
    server_inbox: Result<InboxView, PortError>,
    answers: Result<Vec<PathAnswer>, PortError>,
    upkeep: Result<bool, PortError>,
    calls: Vec<String>,
    opened: u32,
    upkeep_starts: u32,
}

impl State {
    fn new() -> Self {
        let known = KnownSettings {
            member: handle("bob"),
            settings: settings("require-claim"),
        };
        Self {
            now: at(0),
            checkout: true,
            open_error: None,
            repo: Ok(RepoId::parse("api").unwrap()),
            cached_settings: Some(Cached {
                value: known.clone(),
                at: at(0),
            }),
            unreadable_settings: false,
            status: Some(Cached {
                value: StatusView::default(),
                at: at(0),
            }),
            inbox: Some(Cached {
                value: InboxView::default(),
                at: at(0),
            }),
            noted: Vec::new(),
            last_heartbeat: Some(at(0)),
            unreachable: None,
            last_warning: None,
            last_repo_warning: None,
            server_settings: Ok(known),
            server_status: Ok(StatusView::default()),
            server_inbox: Ok(InboxView::default()),
            answers: Ok(Vec::new()),
            upkeep: Ok(true),
            calls: Vec::new(),
            opened: 0,
            upkeep_starts: 0,
        }
    }
}

#[derive(Clone)]
struct World(Rc<RefCell<State>>);

impl World {
    fn new() -> Self {
        Self(Rc::new(RefCell::new(State::new())))
    }

    fn with(self, change: impl FnOnce(&mut State)) -> Self {
        change(&mut self.0.borrow_mut());
        self
    }

    fn calls(&self) -> Vec<String> {
        self.0.borrow().calls.clone()
    }

    fn set(&self, change: impl FnOnce(&mut State)) {
        change(&mut self.0.borrow_mut());
    }

    fn call(&self, name: &str) {
        self.0.borrow_mut().calls.push(name.to_owned());
    }
}

impl Clock for World {
    fn now(&self) -> OffsetDateTime {
        self.0.borrow().now
    }
}

impl Workspaces for World {
    fn open(&self, _cwd: &Path) -> Result<Option<Box<dyn Workspace + '_>>, PortError> {
        let mut state = self.0.borrow_mut();
        state.opened += 1;
        if let Some(error) = state.open_error.clone() {
            return Err(error);
        }
        if !state.checkout {
            return Ok(None);
        }
        Ok(Some(Box::new(self.clone())))
    }
}

impl Workspace for World {
    fn relative(&self, cwd: &Path, path: &Path) -> Option<RelPath> {
        let full = cwd.join(path);
        let inside = full.strip_prefix("/work").ok()?;
        RelPath::parse(&inside.to_string_lossy()).ok()
    }

    fn repo(&self, _settings: &ProjectSettings) -> Result<RepoId, PortError> {
        self.0.borrow().repo.clone()
    }

    fn cache(&self) -> &dyn LocalCache {
        self
    }

    fn gateway(&self) -> &dyn CoordinationGateway {
        self
    }

    fn upkeep(&self) -> &dyn Upkeep {
        self
    }
}

impl LocalCache for World {
    fn settings(&self) -> Result<Option<Cached<KnownSettings>>, PortError> {
        let state = self.0.borrow();
        if state.unreadable_settings {
            return Err(PortError::Refused(
                "the cached settings use a rule this dibs does not know".to_owned(),
            ));
        }
        Ok(state.cached_settings.clone())
    }

    fn status(&self) -> Option<Cached<StatusView>> {
        self.0.borrow().status.clone()
    }

    fn inbox(&self) -> Option<Cached<InboxView>> {
        self.0.borrow().inbox.clone()
    }

    fn noted_holds(&self) -> Vec<Cached<QualifiedArea>> {
        self.0.borrow().noted.clone()
    }

    fn note_holds(&self, areas: &[QualifiedArea], at: OffsetDateTime) {
        let mut state = self.0.borrow_mut();
        for area in areas {
            state.noted.push(Cached {
                value: area.clone(),
                at,
            });
        }
    }

    fn last_heartbeat(&self) -> Option<OffsetDateTime> {
        self.0.borrow().last_heartbeat
    }

    fn unreachable_since(&self) -> Option<OffsetDateTime> {
        self.0.borrow().unreachable
    }

    fn mark_unreachable(&self, at: OffsetDateTime) {
        self.0.borrow_mut().unreachable = Some(at);
    }

    fn clear_unreachable(&self) {
        self.0.borrow_mut().unreachable = None;
    }

    fn last_warning(&self) -> Option<OffsetDateTime> {
        self.0.borrow().last_warning
    }

    fn mark_warning(&self, at: OffsetDateTime) {
        self.0.borrow_mut().last_warning = Some(at);
    }

    fn last_repo_warning(&self) -> Option<OffsetDateTime> {
        self.0.borrow().last_repo_warning
    }

    fn mark_repo_warning(&self, at: OffsetDateTime) {
        self.0.borrow_mut().last_repo_warning = Some(at);
    }
}

impl CoordinationGateway for World {
    fn settings(&self) -> Result<KnownSettings, PortError> {
        self.call("settings");
        let answer = self.0.borrow().server_settings.clone();
        if let Ok(known) = &answer {
            let now = self.now();
            self.0.borrow_mut().cached_settings = Some(Cached {
                value: known.clone(),
                at: now,
            });
        }
        answer
    }

    fn status(&self) -> Result<StatusView, PortError> {
        self.call("status");
        self.0.borrow().server_status.clone()
    }

    fn inbox(&self) -> Result<InboxView, PortError> {
        self.call("inbox");
        self.0.borrow().server_inbox.clone()
    }

    fn edit_check(
        &self,
        session: &SessionRef,
        repo: &RepoId,
        paths: &[RelPath],
    ) -> Result<Vec<PathAnswer>, PortError> {
        let asked: Vec<&str> = paths.iter().map(RelPath::as_str).collect();
        self.call(&format!(
            "edit_check {} {} {repo} {}",
            session.host,
            session.key,
            asked.join(",")
        ));
        self.0.borrow().answers.clone()
    }
}

impl Upkeep for World {
    fn start(&self, _session: &SessionRef, _now: OffsetDateTime) -> Result<bool, PortError> {
        let mut state = self.0.borrow_mut();
        state.upkeep_starts += 1;
        state.upkeep.clone()
    }
}

/// A host with the neutral format and the capabilities a test gives it.
struct Limited(Capabilities);

impl HostAdapter for Limited {
    fn id(&self) -> HostId {
        HostId::new("limited")
    }

    fn capabilities(&self) -> Capabilities {
        self.0
    }

    fn parse(&self, event: &str, input: &[u8]) -> Result<HostEvent, ParseError> {
        Neutral.parse(event, input)
    }

    fn render(&self, event: &str, outcome: &GuardOutcome) -> HostReply {
        Neutral.render(event, outcome)
    }
}

fn registry() -> HostRegistry {
    HostRegistry::new(vec![
        Box::new(Neutral),
        Box::new(Limited(Capabilities::default())),
    ])
}

fn input(tool: Option<Value>) -> Vec<u8> {
    let mut value = json!({"protocol": 1, "session": "s-1", "cwd": "/work", "thread": "main"});
    if let Some(tool) = tool {
        value["tool"] = tool;
    }
    serde_json::to_vec(&value).unwrap()
}

fn edit(paths: &[&str]) -> Vec<u8> {
    input(Some(json!({"kind": "edit", "paths": paths})))
}

fn run(world: &World, host: &str, event: &str, input: &[u8]) -> HookAnswer {
    run_switched(world, host, event, input, false)
}

fn run_switched(world: &World, host: &str, event: &str, input: &[u8], off: bool) -> HookAnswer {
    let guard = Guard::new(world, world);
    guard.answer(
        &registry(),
        &HookRequest {
            host,
            event,
            input,
            switched_off: off,
        },
    )
}

/// The neutral reply as JSON.
fn reply(answer: &HookAnswer) -> Value {
    assert_eq!(answer.reply.exit, 0, "a hook always exits 0");
    serde_json::from_slice(&answer.reply.stdout).unwrap()
}

fn no_objection(answer: &HookAnswer) -> Value {
    let value = reply(answer);
    assert_eq!(value["decision"], "no_objection", "{value}");
    value
}

fn held_by_alice() -> PathAnswer {
    PathAnswer::HeldBy {
        holder: Some(handle("alice")),
        item: ItemId(4),
        area: area("api:billing"),
    }
}

#[test]
fn an_unknown_host_gets_an_empty_reply_and_a_line_for_standard_error() {
    let world = World::new();
    let answer = run(
        &world,
        "elsewhere",
        "before-tool",
        &edit(&["src/billing/a.rs"]),
    );
    assert_eq!(answer.reply, HostReply::empty());
    let diagnostic = answer.diagnostic.unwrap();
    assert!(diagnostic.as_str().contains("'elsewhere'"), "{diagnostic}");
    assert!(
        diagnostic.as_str().contains("neutral, limited"),
        "{diagnostic}"
    );
    assert_eq!(world.0.borrow().opened, 0);
}

#[test]
fn an_unknown_event_or_unreadable_input_is_no_objection_with_a_notice() {
    let world = World::new();
    for (event, input) in [
        ("pre-tool-use", edit(&["a.rs"])),
        ("before-tool", b"{not json".to_vec()),
        (
            "before-tool",
            br#"{"protocol":9,"session":"s","cwd":"/work"}"#.to_vec(),
        ),
    ] {
        let answer = run(&world, "neutral", event, &input);
        let value = no_objection(&answer);
        assert!(
            value["notice"].as_str().unwrap().starts_with("dibs: "),
            "{value}"
        );
        assert_eq!(answer.kind, None);
    }
    assert_eq!(world.0.borrow().opened, 0);
    assert!(world.calls().is_empty());
}

#[test]
fn switched_off_every_event_is_silent_and_touches_no_port() {
    let world = World::new().with(|state| state.last_heartbeat = None);
    for (event, input) in [
        ("session-started", input(None)),
        ("before-tool", edit(&["src/billing/a.rs"])),
        ("after-tool", input(None)),
        ("prompt-submitted", input(None)),
        ("no-such-event", input(None)),
    ] {
        let answer = run_switched(&world, "neutral", event, &input, true);
        assert_eq!(
            reply(&answer),
            json!({"protocol": 1, "decision": "no_objection"})
        );
    }
    let silent = run_switched(&world, "elsewhere", "before-tool", &input(None), true);
    assert_eq!(silent.diagnostic, None);
    let state = world.0.borrow();
    assert_eq!((state.opened, state.upkeep_starts), (0, 0));
    assert!(state.calls.is_empty());
}

#[test]
fn outside_every_checkout_nothing_is_asked() {
    let world = World::new().with(|state| {
        state.checkout = false;
        state.last_heartbeat = None;
    });
    for event in ["session-started", "after-tool"] {
        let answer = run(&world, "neutral", event, &input(None));
        assert_eq!(no_objection(&answer)["notice"], Value::Null);
    }
    let answer = run(
        &world,
        "neutral",
        "before-tool",
        &edit(&["src/billing/a.rs"]),
    );
    assert_eq!(no_objection(&answer)["notice"], Value::Null);
    assert!(world.calls().is_empty());
    assert_eq!(world.0.borrow().upkeep_starts, 0);
}

#[test]
fn ungoverned_paths_and_paths_outside_the_checkout_are_answered_without_the_server() {
    let world = World::new();
    let answer = run(
        &world,
        "neutral",
        "before-tool",
        &edit(&["README.md", "/elsewhere/src/billing/a.rs"]),
    );
    assert_eq!(
        reply(&answer),
        json!({"protocol": 1, "decision": "no_objection"})
    );
    assert!(world.calls().is_empty());
    assert_eq!(answer.kind, Some(EventKind::BeforeTool));
}

#[test]
fn a_held_path_is_denied_naming_the_holder() {
    let world = World::new().with(|state| state.answers = Ok(vec![held_by_alice()]));
    let answer = run(
        &world,
        "neutral",
        "before-tool",
        &edit(&["src/billing/a.rs"]),
    );
    let value = reply(&answer);
    assert_eq!(value["decision"], "deny");
    let reason = value["reason"].as_str().unwrap();
    assert!(reason.contains("alice holds for work item 4"), "{reason}");
    assert_eq!(
        world.calls(),
        ["edit_check neutral s-1 api src/billing/a.rs"]
    );
}

#[test]
fn a_path_that_needs_a_claim_is_denied_with_the_way_to_claim() {
    let world = World::new().with(|state| state.answers = Ok(vec![PathAnswer::NeedsClaim]));
    let answer = run(
        &world,
        "neutral",
        "before-tool",
        &edit(&["src/billing/a.rs"]),
    );
    let value = reply(&answer);
    assert_eq!(value["decision"], "deny");
    assert!(
        value["reason"]
            .as_str()
            .unwrap()
            .contains("dibs claim <item> --area api:billing"),
        "{value}"
    );
}

#[test]
fn only_the_governed_paths_are_asked_and_one_refusal_denies_the_call() {
    let world = World::new().with(|state| {
        state.answers = Ok(vec![PathAnswer::Allowed, held_by_alice()]);
    });
    let answer = run(
        &world,
        "neutral",
        "before-tool",
        &edit(&[
            "README.md",
            "src/billing/a.rs",
            "src/billing/rates.toml",
            "src/billing/a.rs",
        ]),
    );
    assert_eq!(reply(&answer)["decision"], "deny");
    assert_eq!(
        world.calls(),
        ["edit_check neutral s-1 api src/billing/a.rs,src/billing/rates.toml"]
    );
}

#[test]
fn an_allowed_area_is_remembered_and_answers_the_next_edit_without_the_server() {
    let world = World::new().with(|state| state.answers = Ok(vec![PathAnswer::Allowed]));
    run(
        &world,
        "neutral",
        "before-tool",
        &edit(&["src/billing/a.rs"]),
    );
    assert_eq!(world.calls().len(), 1);
    world.set(|state| state.now = at(20));
    let again = run(
        &world,
        "neutral",
        "before-tool",
        &edit(&["src/billing/b.rs"]),
    );
    assert_eq!(no_objection(&again)["notice"], Value::Null);
    assert_eq!(world.calls().len(), 1, "the remembered hold answered");
    world.set(|state| state.now = at(31));
    run(
        &world,
        "neutral",
        "before-tool",
        &edit(&["src/billing/b.rs"]),
    );
    assert_eq!(world.calls().len(), 2, "a remembered hold stands 30 s");
}

#[test]
fn under_an_advisory_policy_an_allowed_area_is_not_taken_for_a_hold() {
    let world = World::new().with(|state| {
        state.answers = Ok(vec![PathAnswer::Allowed]);
        if let Some(cached) = state.cached_settings.as_mut() {
            cached.value.settings = settings("advise");
        }
    });
    run(
        &world,
        "neutral",
        "before-tool",
        &edit(&["src/billing/a.rs"]),
    );
    assert!(world.0.borrow().noted.is_empty());
}

#[test]
fn a_fresh_status_answers_for_my_own_holds_but_not_for_hot_files() {
    let world = World::new().with(|state| {
        state.status = Some(Cached {
            value: StatusView {
                holds: vec![HoldLine {
                    area: area("api:billing"),
                    holder: handle("bob"),
                    item: ItemId(3),
                }],
                ..StatusView::default()
            },
            at: at(-10),
        });
        state.answers = Ok(vec![PathAnswer::Noted {
            rule: "warn-shared".to_owned(),
            note: None,
            touched_by: vec![(handle("alice"), ItemId(4))],
        }]);
    });
    let held = run(
        &world,
        "neutral",
        "before-tool",
        &edit(&["src/billing/a.rs"]),
    );
    assert_eq!(no_objection(&held)["context"], Value::Null);
    assert!(world.calls().is_empty());
    let hot = run(
        &world,
        "neutral",
        "before-tool",
        &edit(&["src/billing/rates.toml"]),
    );
    let context = no_objection(&hot)["context"].as_str().unwrap().to_owned();
    assert!(context.contains("warn-shared rule"), "{context}");
    assert!(context.contains("alice (item 4)"), "{context}");
    assert_eq!(world.calls().len(), 1);
}

#[test]
fn another_members_hold_in_the_status_does_not_answer_for_me() {
    let world = World::new().with(|state| {
        state.status = Some(Cached {
            value: StatusView {
                holds: vec![HoldLine {
                    area: area("api:billing"),
                    holder: handle("alice"),
                    item: ItemId(4),
                }],
                ..StatusView::default()
            },
            at: at(0),
        });
        state.answers = Ok(vec![held_by_alice()]);
    });
    let answer = run(
        &world,
        "neutral",
        "before-tool",
        &edit(&["src/billing/a.rs"]),
    );
    assert_eq!(reply(&answer)["decision"], "deny");
}

#[test]
fn settings_are_fetched_once_when_this_machine_has_none() {
    let world = World::new().with(|state| {
        state.cached_settings = None;
        state.answers = Ok(vec![PathAnswer::Allowed]);
    });
    run(&world, "neutral", "before-tool", &edit(&["README.md"]));
    run(&world, "neutral", "before-tool", &edit(&["README.md"]));
    assert_eq!(world.calls(), ["settings"]);
}

#[test]
fn an_unreachable_server_is_a_notice_then_a_minute_of_silence() {
    let world = World::new().with(|state| {
        state.answers = Err(PortError::Unavailable(
            "the dibs server could not be reached.".to_owned(),
        ));
    });
    let first = run(
        &world,
        "neutral",
        "before-tool",
        &edit(&["src/billing/a.rs"]),
    );
    let notice = no_objection(&first)["notice"].as_str().unwrap().to_owned();
    assert!(notice.contains("could not be reached"), "{notice}");
    assert_eq!(world.0.borrow().unreachable, Some(at(0)));
    world.set(|state| state.now = at(45));
    let within = run(
        &world,
        "neutral",
        "before-tool",
        &edit(&["src/billing/a.rs"]),
    );
    assert_eq!(
        no_objection(&within)["notice"],
        Value::Null,
        "warned once a minute"
    );
    assert_eq!(world.calls().len(), 1, "the circuit kept the server out");
    world.set(|state| {
        state.now = at(61);
        state.answers = Ok(vec![PathAnswer::Allowed]);
    });
    let after = run(
        &world,
        "neutral",
        "before-tool",
        &edit(&["src/billing/a.rs"]),
    );
    assert_eq!(no_objection(&after)["notice"], Value::Null);
    assert_eq!(
        world.calls().len(),
        2,
        "the server is tried again after a minute"
    );
    assert_eq!(
        world.0.borrow().unreachable,
        None,
        "a success closes the circuit"
    );
}

/// A change a test makes to the fakes' state.
type Change = Box<dyn Fn(&mut State)>;

#[test]
fn every_other_failure_is_no_objection_with_its_sentence() {
    let refused = || PortError::Refused("no token is stored for the project.".to_owned());
    let cases: Vec<(&str, Change)> = vec![
        (
            "open",
            Box::new(move |state| state.open_error = Some(refused())),
        ),
        ("repo", Box::new(move |state| state.repo = Err(refused()))),
        (
            "settings",
            Box::new(|state| state.unreadable_settings = true),
        ),
        (
            "server",
            Box::new(move |state| {
                state.cached_settings = None;
                state.server_settings = Err(refused());
            }),
        ),
        (
            "check",
            Box::new(move |state| state.answers = Err(refused())),
        ),
        (
            "answers",
            Box::new(|state| state.answers = Ok(vec![PathAnswer::Allowed, PathAnswer::Allowed])),
        ),
    ];
    for (name, change) in cases {
        let world = World::new();
        world.set(|state| change(state));
        let answer = run(
            &world,
            "neutral",
            "before-tool",
            &edit(&["src/billing/a.rs"]),
        );
        let value = no_objection(&answer);
        assert!(
            value["notice"]
                .as_str()
                .is_some_and(|n| n.starts_with("dibs: ")),
            "{name}: {value}"
        );
        if name != "answers" {
            assert_eq!(
                world.0.borrow().unreachable,
                None,
                "{name}: no circuit for a refusal"
            );
        }
    }
}

#[test]
fn a_checkout_that_governs_no_repository_says_so_at_most_once_a_minute() {
    let world = World::new().with(|state| {
        state.repo = Err(PortError::Refused(
            "the .dibs.toml here names no repo of the project.".to_owned(),
        ));
    });
    let edit_at = |seconds: i64| {
        world.set(|state| state.now = at(seconds));
        no_objection(&run(
            &world,
            "neutral",
            "before-tool",
            &edit(&["src/billing/a.rs"]),
        ))
    };
    let first = edit_at(0);
    assert!(
        first["notice"]
            .as_str()
            .is_some_and(|notice| notice.contains("names no repo")),
        "{first}"
    );
    for seconds in [1, 30, 59] {
        assert_eq!(
            edit_at(seconds),
            json!({"protocol": 1, "decision": "no_objection"}),
            "{seconds} s after the notice"
        );
    }
    let after_a_minute = edit_at(60);
    assert!(after_a_minute["notice"].is_string(), "{after_a_minute}");
    assert!(world.calls().is_empty(), "no edit check was asked for");
    assert_eq!(
        world.0.borrow().last_warning,
        None,
        "the warning about an unreachable server keeps its own minute"
    );
}

#[test]
fn a_host_that_cannot_deny_edits_is_never_asked_about_them() {
    let world = World::new().with(|state| state.answers = Ok(vec![held_by_alice()]));
    let answer = run(
        &world,
        "limited",
        "before-tool",
        &edit(&["src/billing/a.rs"]),
    );
    assert_eq!(
        reply(&answer),
        json!({"protocol": 1, "decision": "no_objection"})
    );
    assert!(world.calls().is_empty());
}

#[test]
fn shell_commands_and_other_tools_are_not_checked_yet() {
    let world = World::new();
    for tool in [
        json!({"kind": "shell", "command": "./deploy.sh"}),
        json!({"kind": "search"}),
    ] {
        let answer = run(&world, "neutral", "before-tool", &input(Some(tool)));
        assert_eq!(
            reply(&answer),
            json!({"protocol": 1, "decision": "no_objection"})
        );
    }
    assert!(world.calls().is_empty());
}

#[test]
fn a_session_starts_with_a_brief_from_a_fresh_cache() {
    let world = World::new().with(|state| {
        state.status = Some(Cached {
            value: StatusView {
                live: vec![crate::ports::ClaimLine {
                    item: ItemId(3),
                    title: "Fix billing".parse().unwrap(),
                    holder: handle("bob"),
                    areas: vec![area("api:billing")],
                }],
                ..StatusView::default()
            },
            at: at(-5),
        });
    });
    let answer = run(&world, "neutral", "session-started", &input(None));
    let context = no_objection(&answer)["context"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(context.contains("you are bob"), "{context}");
    assert!(context.contains("item 3 \"Fix billing\""), "{context}");
    assert!(world.calls().is_empty());
}

#[test]
fn a_stale_cache_is_refreshed_for_the_brief() {
    let world = World::new().with(|state| {
        state.now = at(120);
        state.last_heartbeat = Some(at(120));
        state.server_inbox = Ok(InboxView {
            unread: 1,
            subjects: vec!["Rates".parse().unwrap()],
        });
    });
    let answer = run(&world, "neutral", "session-started", &input(None));
    let context = no_objection(&answer)["context"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(context.contains("Unread conversations: 1"), "{context}");
    assert_eq!(world.calls(), ["status", "inbox"]);
}

#[test]
fn a_brief_that_cannot_be_refreshed_is_given_from_the_old_cache_with_a_notice() {
    let world = World::new().with(|state| {
        state.now = at(600);
        state.server_status = Err(PortError::Unavailable("away.".to_owned()));
    });
    let answer = run(&world, "neutral", "session-started", &input(None));
    let value = no_objection(&answer);
    assert!(
        value["context"]
            .as_str()
            .unwrap()
            .contains("10 minutes ago"),
        "{value}"
    );
    assert!(
        value["notice"].as_str().unwrap().contains("away."),
        "{value}"
    );
}

#[test]
fn a_host_that_cannot_carry_a_brief_gets_none_and_nothing_is_fetched_for_it() {
    let world = World::new().with(|state| state.now = at(600));
    let answer = run(&world, "limited", "session-started", &input(None));
    assert_eq!(no_objection(&answer)["context"], Value::Null);
    assert!(world.calls().is_empty());
}

#[test]
fn upkeep_starts_only_when_something_is_due() {
    let world = World::new().with(|state| state.now = at(29));
    for event in ["after-tool", "prompt-submitted"] {
        run(&world, "neutral", event, &input(None));
    }
    assert_eq!(world.0.borrow().upkeep_starts, 0, "everything is fresh");
    world.set(|state| state.now = at(30));
    run(&world, "neutral", "after-tool", &input(None));
    assert_eq!(world.0.borrow().upkeep_starts, 1, "the status is due");
    assert!(world.calls().is_empty(), "upkeep is never waited for");
}

#[test]
fn the_heartbeat_follows_the_projects_interval_from_the_cached_settings() {
    let world = World::new().with(|state| {
        state.now = at(100);
        state.status = Some(Cached {
            value: StatusView::default(),
            at: at(100),
        });
        state.inbox = Some(Cached {
            value: InboxView::default(),
            at: at(100),
        });
        state.cached_settings.as_mut().unwrap().at = at(100);
    });
    run(&world, "neutral", "after-tool", &input(None));
    assert_eq!(
        world.0.borrow().upkeep_starts,
        0,
        "two minutes have not passed"
    );
    world.set(|state| {
        state.now = at(120);
        state.status.as_mut().unwrap().at = at(120);
        state.inbox.as_mut().unwrap().at = at(120);
    });
    run(&world, "neutral", "after-tool", &input(None));
    assert_eq!(world.0.borrow().upkeep_starts, 1);
}

#[test]
fn no_upkeep_starts_before_a_tool_or_while_the_server_is_unreachable() {
    let world = World::new().with(|state| {
        state.last_heartbeat = None;
        state.answers = Ok(vec![PathAnswer::Allowed]);
    });
    run(
        &world,
        "neutral",
        "before-tool",
        &edit(&["src/billing/a.rs"]),
    );
    assert_eq!(world.0.borrow().upkeep_starts, 0);
    world.set(|state| state.unreachable = Some(at(0)));
    run(&world, "neutral", "after-tool", &input(None));
    assert_eq!(world.0.borrow().upkeep_starts, 0);
}

#[test]
fn upkeep_that_cannot_start_is_a_notice() {
    let world = World::new().with(|state| {
        state.last_heartbeat = None;
        state.upkeep = Err(PortError::Refused(
            "the state directory is read-only.".to_owned(),
        ));
    });
    let answer = run(&world, "neutral", "after-tool", &input(None));
    let notice = no_objection(&answer)["notice"].as_str().unwrap().to_owned();
    assert!(notice.contains("read-only"), "{notice}");
}
