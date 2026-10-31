//! The guard's outcome, written as the JSON output Claude Code reads from a command hook.

use serde::Serialize;

use dibs_agent_host::adapter::HostReply;
use dibs_agent_host::outcome::GuardOutcome;

use crate::event::HookEvent;

/// The only permission decision this adapter writes.
const DENY: &str = "deny";

/// A hook's output: the event's own fields and the line for the person.
#[derive(Serialize)]
struct Output<'a> {
    #[serde(rename = "hookSpecificOutput", skip_serializing_if = "Option::is_none")]
    specific: Option<Specific<'a>>,
    #[serde(rename = "systemMessage", skip_serializing_if = "Option::is_none")]
    system_message: Option<String>,
}

/// `hookSpecificOutput`.
#[derive(Serialize)]
struct Specific<'a> {
    #[serde(rename = "hookEventName")]
    hook_event_name: &'static str,
    #[serde(rename = "permissionDecision", skip_serializing_if = "Option::is_none")]
    permission_decision: Option<&'static str>,
    #[serde(
        rename = "permissionDecisionReason",
        skip_serializing_if = "Option::is_none"
    )]
    permission_decision_reason: Option<&'a str>,
    #[serde(rename = "additionalContext", skip_serializing_if = "Option::is_none")]
    additional_context: Option<&'a str>,
}

impl<'a> Specific<'a> {
    fn new(event: HookEvent) -> Self {
        Self {
            hook_event_name: event.name,
            permission_decision: None,
            permission_decision_reason: None,
            additional_context: None,
        }
    }
}

/// The reply carrying `outcome` for `event`, `None` when the event is not one Claude Code sends
/// to dibs. Such a reply carries only what every event accepts: the line for the person.
pub fn render(event: Option<HookEvent>, outcome: &GuardOutcome) -> HostReply {
    let output = match outcome {
        GuardOutcome::Deny { reason } => match event.filter(|event| event.can_deny()) {
            Some(event) => Output {
                specific: Some(Specific {
                    permission_decision: Some(DENY),
                    permission_decision_reason: Some(reason.as_str()),
                    ..Specific::new(event)
                }),
                system_message: None,
            },
            None => Output {
                specific: None,
                system_message: Some(reason.one_line()),
            },
        },
        GuardOutcome::NoObjection { context, notice } => Output {
            specific: event
                .filter(|event| event.carries_context)
                .zip(context.as_ref())
                .map(|(event, context)| Specific {
                    additional_context: Some(context.as_str()),
                    ..Specific::new(event)
                }),
            system_message: notice.as_ref().map(|notice| notice.one_line()),
        },
        GuardOutcome::Worktree { notice, .. } => Output {
            specific: None,
            system_message: notice.as_ref().map(|notice| notice.one_line()),
        },
        GuardOutcome::NoWorktree { reason } => Output {
            specific: None,
            system_message: Some(reason.one_line()),
        },
    };
    if output.specific.is_none() && output.system_message.is_none() {
        return HostReply::empty();
    }
    let mut stdout = serde_json::to_vec(&output).unwrap_or_default();
    if !stdout.is_empty() {
        stdout.push(b'\n');
    }
    HostReply::stdout(stdout)
}
