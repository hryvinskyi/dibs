//! The conformance kit: what every adapter must do, checked over fixtures of its host's native
//! format. A new host is accepted when its fixtures pass.
//!
//! An adapter's tests call [`check`] with
//! - pairs of host-native input and the neutral event it must parse to (or must refuse),
//! - for a set of outcomes, each on one of the host's events, what the host-native reply must do:
//!   block or not, and which text it carries,
//! - a [`ReplyReader`] that reads the host's reply back, written in the adapter's tests.
//!
//! Beyond the fixtures, every adapter is held to rules no host is exempt from: its id is a valid
//! agent label; an event name it does not know, empty input and input that is not its format are
//! refused rather than guessed at; no reply exits with status `2`, which hosts read as a block; a
//! reply carries no text for the agent when the host declares no way to carry it; an outcome
//! rendered for an event the host does not send is still a reply the host can read.

use dibs_domain::session::AgentLabel;

use crate::adapter::{HostAdapter, HostReply};
use crate::event::HostEvent;
use crate::outcome::GuardOutcome;

/// An event name no host sends.
const NO_SUCH_EVENT: &str = "dibs-conformance-no-such-event";

/// One host-native input and what it must parse to.
#[derive(Debug, Clone)]
pub struct EventFixture {
    /// The event name the hook configuration passes.
    pub event: String,
    /// The host's input.
    pub input: Vec<u8>,
    /// The event it must parse to; `None` when it must be refused.
    pub expected: Option<HostEvent>,
}

impl EventFixture {
    /// Input that must parse to `expected`.
    pub fn parses(event: &str, input: &[u8], expected: HostEvent) -> Self {
        Self {
            event: event.to_owned(),
            input: input.to_vec(),
            expected: Some(expected),
        }
    }

    /// Input that must be refused.
    pub fn refuses(event: &str, input: &[u8]) -> Self {
        Self {
            event: event.to_owned(),
            input: input.to_vec(),
            expected: None,
        }
    }
}

/// An outcome and what the host's reply must do with it.
#[derive(Debug, Clone)]
pub struct ReplyFixture {
    /// The event the outcome answers, as the hook configuration names it; empty for a host
    /// whose reply does not depend on its event.
    pub event: String,
    /// The outcome rendered.
    pub outcome: GuardOutcome,
    /// Whether the reply must stop the tool.
    pub denies: bool,
    /// The reason it must carry, when it denies.
    pub reason: Option<String>,
    /// The text for the agent it must carry.
    pub context: Option<String>,
    /// The line for the person it must carry.
    pub notice: Option<String>,
}

impl ReplyFixture {
    /// The outcome, its reply expected to let the tool run and carry nothing.
    pub fn outcome(outcome: GuardOutcome) -> Self {
        Self {
            event: String::new(),
            outcome,
            denies: false,
            reason: None,
            context: None,
            notice: None,
        }
    }

    /// The outcome answers the event of this name.
    #[must_use]
    pub fn on(mut self, event: &str) -> Self {
        self.event = event.to_owned();
        self
    }

    /// The reply lets the tool run.
    #[must_use]
    pub fn allows(mut self) -> Self {
        self.denies = false;
        self
    }

    /// The reply stops the tool with this reason.
    #[must_use]
    pub fn denies(mut self, reason: &str) -> Self {
        self.denies = true;
        self.reason = Some(reason.to_owned());
        self
    }

    /// The reply carries this text for the agent.
    #[must_use]
    pub fn carries_context(mut self, context: &str) -> Self {
        self.context = Some(context.to_owned());
        self
    }

    /// The reply carries this line for the person.
    #[must_use]
    pub fn carries_notice(mut self, notice: &str) -> Self {
        self.notice = Some(notice.to_owned());
        self
    }
}

/// An adapter's fixtures.
#[derive(Debug, Clone, Default)]
pub struct Fixtures {
    /// Inputs and their events.
    pub events: Vec<EventFixture>,
    /// Outcomes and their replies.
    pub replies: Vec<ReplyFixture>,
}

/// A host's reply, read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadReply {
    /// Whether it stops the tool.
    pub denies: bool,
    /// The reason it carries.
    pub reason: Option<String>,
    /// The text for the agent it carries.
    pub context: Option<String>,
    /// The line for the person it carries.
    pub notice: Option<String>,
}

/// Reads a host's reply back, as the host would.
pub trait ReplyReader {
    /// The reply as the host reads it; `None` when the host could not read it at all.
    fn read(&self, reply: &HostReply) -> Option<ReadReply>;
}

/// Checks an adapter against its fixtures and the rules every adapter keeps.
///
/// # Errors
/// One sentence per broken expectation.
pub fn check(
    adapter: &dyn HostAdapter,
    fixtures: &Fixtures,
    reader: &dyn ReplyReader,
) -> Result<(), Vec<String>> {
    let mut broken = Vec::new();
    let id = adapter.id();
    if AgentLabel::parse(id.as_str()).is_err() {
        broken.push(format!(
            "the id '{id}' is not an agent label (1 to 32 of a-z, 0-9 and '-')"
        ));
    }
    if adapter.parse(NO_SUCH_EVENT, b"{}").is_ok() {
        broken.push("an unknown event name was read as an event".to_owned());
    }
    let mut names: Vec<&str> = fixtures.events.iter().map(|f| f.event.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    for name in names {
        for junk in [&b""[..], b"not the host's format", b"\xff\xfe"] {
            if adapter.parse(name, junk).is_ok() {
                broken.push(format!(
                    "{name}: input that is not the host's format was read as an event"
                ));
            }
        }
    }
    for fixture in &fixtures.events {
        check_event(adapter, fixture, &mut broken);
    }
    let capabilities = adapter.capabilities();
    let carries_context = capabilities.context_at_start
        || capabilities.context_after_tool
        || capabilities.context_at_prompt;
    for fixture in &fixtures.replies {
        let reply = adapter.render(&fixture.event, &fixture.outcome);
        check_reply(fixture, &reply, reader, carries_context, &mut broken);
        let elsewhere = adapter.render(NO_SUCH_EVENT, &fixture.outcome);
        if elsewhere.exit == 2 || reader.read(&elsewhere).is_none() {
            broken.push(format!(
                "{:?} for an event the host does not send: the reply exits 2 or cannot be read",
                fixture.outcome
            ));
        }
    }
    if broken.is_empty() {
        Ok(())
    } else {
        Err(broken)
    }
}

fn check_event(adapter: &dyn HostAdapter, fixture: &EventFixture, broken: &mut Vec<String>) {
    let shown = String::from_utf8_lossy(&fixture.input);
    let parsed = adapter.parse(&fixture.event, &fixture.input);
    match (&fixture.expected, parsed) {
        (Some(expected), Ok(event)) if event == *expected => {}
        (Some(expected), Ok(event)) => broken.push(format!(
            "{} {shown}: read as {event:?}, expected {expected:?}",
            fixture.event
        )),
        (Some(_), Err(error)) => broken.push(format!(
            "{} {shown}: refused ({error}), expected an event",
            fixture.event
        )),
        (None, Ok(event)) => broken.push(format!(
            "{} {shown}: read as {event:?}, expected a refusal",
            fixture.event
        )),
        (None, Err(_)) => {}
    }
}

fn check_reply(
    fixture: &ReplyFixture,
    reply: &HostReply,
    reader: &dyn ReplyReader,
    carries_context: bool,
    broken: &mut Vec<String>,
) {
    let what = if fixture.event.is_empty() {
        format!("{:?}", fixture.outcome)
    } else {
        format!("{} {:?}", fixture.event, fixture.outcome)
    };
    if reply.exit == 2 {
        broken.push(format!(
            "{what}: the reply exits 2, which hosts read as a block"
        ));
    }
    let Some(read) = reader.read(reply) else {
        broken.push(format!("{what}: the host could not read the reply"));
        return;
    };
    if read.denies != fixture.denies {
        broken.push(format!(
            "{what}: the reply {} the tool, expected it to {}",
            if read.denies { "stops" } else { "lets through" },
            if fixture.denies {
                "stop it"
            } else {
                "let it through"
            }
        ));
    }
    if fixture.denies && read.reason != fixture.reason {
        broken.push(format!(
            "{what}: the reason read back is {:?}, expected {:?}",
            read.reason, fixture.reason
        ));
    }
    let context = if carries_context {
        fixture.context.clone()
    } else {
        None
    };
    if read.context != context {
        broken.push(format!(
            "{what}: the text for the agent read back is {:?}, expected {context:?}",
            read.context
        ));
    }
    if read.notice != fixture.notice {
        broken.push(format!(
            "{what}: the line for the person read back is {:?}, expected {:?}",
            read.notice, fixture.notice
        ));
    }
}
