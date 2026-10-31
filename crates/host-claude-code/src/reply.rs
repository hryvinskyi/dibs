//! The guard's outcome, written as the output Claude Code reads from a command hook: a JSON object
//! for most events, a directory's path for the event that asks for a worktree.

use std::path::Path;

use serde::Serialize;

use dibs_agent_host::adapter::HostReply;
use dibs_agent_host::outcome::GuardOutcome;
use dibs_agent_host::text::Text;

use crate::event::{HookEvent, ReplyShape};

/// The only permission decision this adapter writes.
const DENY: &str = "deny";

/// The exit status that fails the creation of a worktree. Claude Code reads any status but `0` as
/// a failed creation, and `2` as a block, which no reply uses.
const NOT_MADE: u8 = 1;

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

/// The reply carrying `outcome` for `event`, in the shape Claude Code reads for it; `None` when
/// the event is not one Claude Code sends to dibs. Such a reply is a JSON object carrying only what
/// every event accepts: the line for the person.
pub fn render(event: Option<HookEvent>, outcome: &GuardOutcome) -> HostReply {
    match event.map_or(ReplyShape::Json, |event| event.reply) {
        ReplyShape::Json => json(event, outcome),
        ReplyShape::Path => path(outcome),
    }
}

/// The JSON object carrying `outcome` for `event`. A worktree or its refusal, which no event of
/// this shape asks for, is only its line for the person.
fn json(event: Option<HookEvent>, outcome: &GuardOutcome) -> HostReply {
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

/// The reply to a worktree wanted: the directory as the only line of standard output and exit `0`,
/// with the line for the person on standard error. A refusal is its reason on standard error and
/// a failing status, which fails the creation. No objection names no directory: it exits `0` with
/// nothing on standard output, its line for the person on standard error, and so leaves the
/// worktree to whoever runs the hook, as a hook that answers nothing does.
fn path(outcome: &GuardOutcome) -> HostReply {
    match outcome {
        GuardOutcome::Worktree { path, notice } => HostReply {
            stdout: line_of(path),
            stderr: notice.as_ref().map(said).unwrap_or_default(),
            exit: 0,
        },
        GuardOutcome::NoWorktree { reason } | GuardOutcome::Deny { reason } => HostReply {
            stdout: Vec::new(),
            stderr: said(reason),
            exit: NOT_MADE,
        },
        GuardOutcome::NoObjection { notice, .. } => HostReply {
            stdout: Vec::new(),
            stderr: notice.as_ref().map(said).unwrap_or_default(),
            exit: 0,
        },
    }
}

/// The path as one line.
fn line_of(path: &Path) -> Vec<u8> {
    let mut line = path.as_os_str().as_encoded_bytes().to_vec();
    line.push(b'\n');
    line
}

/// The text as one line for the person.
fn said(text: &Text) -> Vec<u8> {
    let mut line = text.one_line().into_bytes();
    line.push(b'\n');
    line
}
