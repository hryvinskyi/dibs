//! The conformance kit over the `neutral` host, as every adapter's tests run it over theirs, and
//! over adapters that break the rules it holds every adapter to.

use dibs_agent_host::adapter::{
    HostAdapter, HostId, HostReply, OwnWorktrees, ParseError, WorktreeTerms,
};
use dibs_agent_host::capabilities::Capabilities;
use dibs_agent_host::conformance::{
    self, Fixtures, ReadReply, ReadWorktree, ReplyFixture, ReplyReader,
};
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
            return HostReply::stdout(b"not a reply".to_vec());
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

/// The neutral host, declaring or not that it serves worktrees, with terms or without, and
/// rendering a wanted worktree's answers well or badly.
struct Serving {
    declares: bool,
    terms: Option<WorktreeTerms>,
    renders_well: bool,
}

const TERMS: WorktreeTerms = WorktreeTerms {
    hook_timeout: std::time::Duration::from_secs(900),
    own: OwnWorktrees {
        dir: "tool/worktrees",
        branch_prefix: "worktree-",
    },
};

impl HostAdapter for Serving {
    fn id(&self) -> HostId {
        HostId::new("serving")
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            serves_worktrees: self.declares,
            ..Neutral.capabilities()
        }
    }

    fn parse(&self, event: &str, input: &[u8]) -> Result<HostEvent, ParseError> {
        Neutral.parse(event, input)
    }

    fn render(&self, event: &str, outcome: &GuardOutcome) -> HostReply {
        match outcome {
            GuardOutcome::Worktree { path, .. } if self.renders_well => {
                HostReply::stdout(format!("{}\n", path.display()).into_bytes())
            }
            GuardOutcome::NoWorktree { reason } if self.renders_well => HostReply {
                stdout: Vec::new(),
                stderr: reason.as_str().as_bytes().to_vec(),
                exit: 1,
            },
            GuardOutcome::NoWorktree { reason } => HostReply {
                stdout: Vec::new(),
                stderr: reason.as_str().as_bytes().to_vec(),
                exit: 2,
            },
            other => Neutral.render(event, other),
        }
    }

    fn worktree_terms(&self) -> Option<WorktreeTerms> {
        self.terms
    }
}

/// Reads the neutral reply, and a wanted worktree's answer as `Serving` writes it.
struct ServingReader;

impl ReplyReader for ServingReader {
    fn read(&self, reply: &HostReply) -> Option<ReadReply> {
        NeutralReader.read(reply)
    }

    fn read_worktree(&self, reply: &HostReply) -> Option<ReadWorktree> {
        if reply.exit == 0 {
            let stdout = String::from_utf8(reply.stdout.clone()).ok()?;
            let last = stdout.lines().last()?;
            return last
                .starts_with('/')
                .then(|| ReadWorktree::Path(last.to_owned()));
        }
        String::from_utf8(reply.stderr.clone())
            .ok()
            .map(ReadWorktree::Refused)
    }
}

#[test]
fn serving_worktrees_is_declared_exactly_by_an_adapter_with_terms_that_renders_both_answers() {
    let check = |adapter: &Serving, fixtures: &Fixtures| {
        conformance::check(adapter, fixtures, &ServingReader)
            .err()
            .unwrap_or_default()
            .join("\n")
    };
    let mut fixtures = neutral::fixtures();
    fixtures.replies.retain(|fixture| {
        !matches!(
            fixture.outcome,
            GuardOutcome::Worktree { .. } | GuardOutcome::NoWorktree { .. }
        )
    });
    let well = Serving {
        declares: true,
        terms: Some(TERMS),
        renders_well: true,
    };
    assert_eq!(check(&well, &fixtures), "");
    assert!(
        check(
            &Serving {
                terms: None,
                ..well
            },
            &fixtures
        )
        .contains("serves_worktrees is declared, but the adapter declares no worktree terms")
    );
    assert!(
        check(
            &Serving {
                declares: false,
                ..well
            },
            &fixtures
        )
        .contains("the adapter declares worktree terms, but serves_worktrees is not declared")
    );
    let badly = check(
        &Serving {
            renders_well: false,
            ..well
        },
        &fixtures,
    );
    for expected in [
        "the host does not read the worktree's directory back",
        "the reply refusing a worktree exits 2",
    ] {
        assert!(
            badly.contains(expected),
            "missing '{expected}' in:\n{badly}"
        );
    }
    let outside = Serving {
        terms: Some(WorktreeTerms {
            hook_timeout: std::time::Duration::from_secs(60),
            own: OwnWorktrees {
                dir: "../elsewhere",
                branch_prefix: "",
            },
        }),
        ..well
    };
    let terms = check(&outside, &fixtures);
    for expected in [
        "leaves a take no time",
        "is not a directory inside the repository",
        "makes no branch name",
    ] {
        assert!(
            terms.contains(expected),
            "missing '{expected}' in:\n{terms}"
        );
    }
    let mut unwanted = fixtures.clone();
    unwanted
        .events
        .retain(|fixture| fixture.event != "worktree-wanted");
    assert!(check(&well, &unwanted).contains("no fixture reads an event as a wanted worktree"));
}
