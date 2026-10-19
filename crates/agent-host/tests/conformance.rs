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

/// An adapter that breaks every rule the kit holds adapters to.
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

    fn render(&self, outcome: &GuardOutcome) -> HostReply {
        let mut reply = Neutral.render(outcome);
        if outcome.denies() {
            reply.exit = 2;
        }
        reply
    }
}

#[test]
fn an_adapter_that_guesses_blocks_with_exit_2_or_carries_what_it_cannot_is_refused() {
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
