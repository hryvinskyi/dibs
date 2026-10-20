//! Claude Code's hook input, read as a neutral event.

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::Value;

use dibs_agent_host::adapter::ParseError;
use dibs_agent_host::event::{EventKind, HostEvent, SessionKey, Thread, ToolCall};

use crate::event::HookEvent;

/// The fields of the input the guard reads; every other field is ignored.
#[derive(Deserialize)]
struct Input {
    session_id: String,
    cwd: String,
    #[serde(default)]
    hook_event_name: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    tool_input: Option<Value>,
}

/// What a tool does, as far as the guard tells tools apart.
enum Shape {
    /// It writes the file named by the first of these `tool_input` fields that is present.
    Edit(&'static [&'static str]),
    /// It runs the shell command line in `tool_input.command`.
    Shell,
}

/// The tools the guard reads by name. A tool not listed is one it has no say about.
const TOOLS: [(&str, Shape); 5] = [
    ("Edit", Shape::Edit(&["file_path"])),
    ("Write", Shape::Edit(&["file_path"])),
    ("MultiEdit", Shape::Edit(&["file_path"])),
    ("NotebookEdit", Shape::Edit(&["notebook_path", "file_path"])),
    ("Bash", Shape::Shell),
];

fn malformed(reason: impl Into<String>) -> ParseError {
    ParseError::Malformed(reason.into())
}

/// The neutral event the input of `hook` is.
///
/// # Errors
/// [`ParseError::Malformed`] when the input is not a hook input of Claude Code, names another
/// event, or lacks what the event needs.
pub fn parse(hook: HookEvent, input: &[u8]) -> Result<HostEvent, ParseError> {
    let input: Input = serde_json::from_slice(input)
        .map_err(|error| malformed(format!("not a Claude Code hook input ({error})")))?;
    if let Some(name) = input.hook_event_name.as_deref()
        && name != hook.name
    {
        return Err(malformed(format!(
            "the input is for the event {name}, but the hook was run as {}",
            hook.argument
        )));
    }
    let session_key = SessionKey::parse(&input.session_id)
        .ok_or_else(|| malformed("session_id is 1 to 256 characters with no control characters"))?;
    if input.cwd.is_empty() {
        return Err(malformed("cwd is empty"));
    }
    let cwd = PathBuf::from(&input.cwd);
    let thread = if input.agent_id.as_deref().is_some_and(|id| !id.is_empty()) {
        Thread::Subagent
    } else {
        Thread::Main
    };
    Ok(match hook.kind {
        EventKind::SessionStarted => HostEvent::SessionStarted { session_key, cwd },
        EventKind::BeforeTool => HostEvent::BeforeTool {
            session_key,
            cwd,
            call: tool_call(input.tool_name.as_deref(), input.tool_input.as_ref())?,
            thread,
        },
        EventKind::AfterTool => HostEvent::AfterTool {
            session_key,
            cwd,
            thread,
        },
        EventKind::PromptSubmitted => HostEvent::PromptSubmitted {
            session_key,
            cwd,
            thread,
        },
    })
}

/// The tool about to run.
fn tool_call(name: Option<&str>, tool_input: Option<&Value>) -> Result<ToolCall, ParseError> {
    let name = name.ok_or_else(|| malformed("tool_name is missing"))?;
    let Some((_, shape)) = TOOLS.iter().find(|(tool, _)| *tool == name) else {
        return Ok(ToolCall::Other);
    };
    let text = |field: &str| {
        tool_input
            .and_then(|fields| fields.get(field))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    };
    match shape {
        Shape::Edit(fields) => {
            let path = fields.iter().find_map(|field| text(field)).ok_or_else(|| {
                malformed(format!(
                    "the {name} tool's input has no {}",
                    fields.join(" or ")
                ))
            })?;
            Ok(ToolCall::Edit {
                paths: vec![PathBuf::from(path)],
            })
        }
        Shape::Shell => {
            let command = text("command")
                .ok_or_else(|| malformed(format!("the {name} tool's input has no command")))?;
            Ok(ToolCall::Shell {
                command: command.to_owned(),
            })
        }
    }
}
