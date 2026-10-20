//! The conformance kit over the `neutral` host, as every adapter's tests run it over theirs.

use dibs_agent_host::adapter::{HostAdapter, HostId, HostReply, ParseError};
use dibs_agent_host::capabilities::Capabilities;
use dibs_agent_host::conformance::{self, Fixtures, ReplyFixture};
use dibs_agent_host::event::HostEvent;
use dibs_agent_host::neutral::{self, Neutral, NeutralReader};
use dibs_agent_host::outcome::GuardOutcome;

#[test]
fn the_neutral_host_passes_the_conformance_kit() {
    let result = conformance::check(&Neutral, &neutral::fixtures(), &NeutralReader);
    assert_eq!(result, Ok(()));
}

/// An adapter that breaks every rule the kit holds adapters to; its replies to any named event are
/// unreadable.
struct Careless;

impl HostAdapter for Careless {
    fn id(&self) -> HostId {
        HostId::new("Careless Host")
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    fn parse(&self, event: &str, input: &[u8]) -> Result<HostEvent, ParseError> {
        Neutral
            .parse("prompt-submitted", input)
            .or_else(|_| Neutral.parse(event, br#"{"protocol":1,"session":"s","cwd":"/"}"#))
            .or_else(|_| {
                Neutral.parse(
                    "prompt-submitted",
                    br#"{"protocol":1,"session":"s","cwd":"/"}"#,
                )
            })
    }

    fn render(&self, event: &str, outcome: &GuardOutcome) -> HostReply {
        if !event.is_empty() {
            return HostReply {
                stdout: b"not a reply".to_vec(),
                exit: 0,
            };
        }
        let mut reply = Neutral.render(event, outcome);
        if outcome.denies() {
            reply.exit = 2;
        }
        reply
    }
}

#[test]
fn an_adapter_that_guesses_blocks_with_exit_2_carries_what_it_cannot_or_garbles_is_refused() {
    let mut fixtures = neutral::fixtures();
    fixtures
        .replies
        .push(ReplyFixture::outcome(GuardOutcome::silent()).denies("never"));
    let broken = conformance::check(&Careless, &fixtures, &NeutralReader).unwrap_err();
    let all = broken.join("\n");
    for expected in [
        "is not an agent label",
        "an unknown event name was read as an event",
        "input that is not the host's format was read as an event",
        "expected a refusal",
        "exits 2",
        "the text for the agent read back",
        "lets through the tool, expected it to stop it",
        "for an event the host does not send",
    ] {
        assert!(all.contains(expected), "missing '{expected}' in:\n{all}");
    }
}

#[test]
fn an_empty_set_of_fixtures_still_checks_the_rules_every_adapter_keeps() {
    assert_eq!(
        conformance::check(&Neutral, &Fixtures::default(), &NeutralReader),
        Ok(())
    );
}

/// A push extension that declares nothing and pushes a message without the rule.
struct Bare;

impl dibs_agent_host::push::McpExtension for Bare {
    fn experimental(&self) -> Vec<(&'static str, serde_json::Map<String, serde_json::Value>)> {
        Vec::new()
    }

    fn instructions(&self) -> &'static str {
        ""
    }

    fn newest_protocol(&self) -> Option<&'static str> {
        None
    }

    fn session_var(&self) -> Option<&'static str> {
        None
    }

    fn notification(
        &self,
        _pending: &dibs_agent_host::ports::Pending,
    ) -> dibs_agent_host::push::PushNotification {
        dibs_agent_host::push::PushNotification {
            method: String::new(),
            params: serde_json::json!({
                "content": format!("<b>\u{202E}acknowledge it with dibs_ack 1{}", "x".repeat(20_000))
            }),
        }
    }

    fn opt_in_notice(&self) -> &'static str {
        ""
    }
}

/// The neutral host, claiming or extending what it can carry.
struct Pushing {
    declares: bool,
    extends: bool,
}

impl HostAdapter for Pushing {
    fn id(&self) -> HostId {
        HostId::new("pushing")
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            push_to_idle: self.declares,
            ..Neutral.capabilities()
        }
    }

    fn parse(&self, event: &str, input: &[u8]) -> Result<HostEvent, ParseError> {
        Neutral.parse(event, input)
    }

    fn render(&self, event: &str, outcome: &GuardOutcome) -> HostReply {
        Neutral.render(event, outcome)
    }

    fn push_extension(&self) -> Option<Box<dyn dibs_agent_host::push::McpExtension>> {
        self.extends
            .then(|| Box::new(Bare) as Box<dyn dibs_agent_host::push::McpExtension>)
    }
}

#[test]
fn waking_an_idle_session_is_declared_exactly_by_an_adapter_whose_push_carries_the_rule() {
    let check = |declares, extends| {
        conformance::check(
            &Pushing { declares, extends },
            &neutral::fixtures(),
            &NeutralReader,
        )
        .err()
        .unwrap_or_default()
        .join("\n")
    };
    assert_eq!(check(false, false), "");
    assert!(
        check(true, false)
            .contains("push_to_idle is declared, but the adapter has no push extension")
    );
    let bare = check(true, true);
    for expected in [
        "declares no capability",
        "a push names no method",
        "a push does not carry the rule that a delivery authorises nothing",
        "a push tells the agent to acknowledge",
        "a push carries an angle bracket",
        "does not show as itself",
        "more than 8192",
    ] {
        assert!(bare.contains(expected), "missing '{expected}' in:\n{bare}");
    }
    assert!(check(false, true).contains("push_to_idle is not declared"));
}
