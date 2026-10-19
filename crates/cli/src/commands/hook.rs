//! `dibs hook <host> <event>`: the guard answering one hook of an agent host.
//!
//! It always ends the way the host's adapter says "no objection" unless the guard denies, and it
//! never exits with status `2`: an unknown host or event, input it cannot read, a command line
//! clap refuses, a checkout or credential that cannot be used — each is an answer of no objection
//! with a notice. `DIBS_OFF` makes every answer a silent no objection without any network call.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::ExitCode;
use std::time::Instant;

use time::{Duration, OffsetDateTime};

use dibs_agent_host::{Guard, HookRequest};
use dibs_client::locations::Locations;

use crate::guard::workspace::ClientWorkspaces;
use crate::guard::{OFF_VAR, SystemClock, switched_off, timing};
use crate::hosts;

/// The most of standard input a hook reads; input beyond it is cut and fails to parse.
const MAX_INPUT: u64 = 8 * 1024 * 1024;

/// Runs one hook and answers its exit status.
pub fn hook(host: &str, event: &str, env: &HashMap<String, String>, started: Instant) -> ExitCode {
    let mut input = Vec::new();
    if let Err(error) = std::io::stdin().take(MAX_INPUT).read_to_end(&mut input) {
        tracing::debug!(error = %error, "the hook's input could not be read");
    }
    let registry = hosts::registry();
    let workspaces = ClientWorkspaces::new(env.clone());
    let clock = SystemClock;
    let guard = Guard::new(&workspaces, &clock);
    let answer = guard.answer(
        &registry,
        &HookRequest {
            host,
            event,
            input: &input,
            switched_off: switched_off(env.get(OFF_VAR).map(String::as_str)),
        },
    );
    let mut stdout = std::io::stdout().lock();
    if let Err(error) = stdout
        .write_all(&answer.reply.stdout)
        .and_then(|()| stdout.flush())
    {
        tracing::debug!(error = %error, "the hook's reply could not be written");
    }
    if let Some(diagnostic) = &answer.diagnostic {
        eprintln!("{}", diagnostic.one_line());
    }
    if let Some(kind) = answer.kind {
        record(env, host, kind, started);
    }
    ExitCode::from(exit_status(answer.reply.exit))
}

/// The status a hook exits with: the adapter's, except that `2` — which hosts read as a block —
/// is never one.
fn exit_status(exit: u8) -> u8 {
    if exit == 2 { 0 } else { exit }
}

fn record(
    env: &HashMap<String, String>,
    host: &str,
    kind: dibs_agent_host::EventKind,
    started: Instant,
) {
    let Ok(locations) = Locations::from_env(&|name| env.get(name).cloned()) else {
        return;
    };
    let took = Duration::try_from(started.elapsed()).unwrap_or(Duration::MAX);
    let day = OffsetDateTime::now_utc().date();
    if let Err(error) = timing::record(locations.state_dir(), day, host, kind, took) {
        tracing::debug!(error = %error, "the hook's duration could not be recorded");
    }
}

/// The host and event of a command line that asks for a hook, when clap could not read it: the
/// first words that are not options, after `hook`. Missing words are empty, which the guard
/// answers as an unknown host or event.
pub fn hook_words(args: &[std::ffi::OsString]) -> Option<(String, String)> {
    let mut words = args
        .iter()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .filter(|arg| !arg.starts_with('-'));
    if words.next().as_deref() != Some("hook") {
        return None;
    }
    let host = words.next().unwrap_or_default();
    let event = words.next().unwrap_or_default();
    Some((host, event))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn args(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    #[test]
    fn a_hook_is_recognised_whatever_else_the_line_holds() {
        assert_eq!(
            hook_words(&args(&[
                "dibs",
                "--json",
                "hook",
                "neutral",
                "after-tool",
                "--extra"
            ])),
            Some(("neutral".to_owned(), "after-tool".to_owned()))
        );
        assert_eq!(
            hook_words(&args(&["dibs", "hook"])),
            Some((String::new(), String::new()))
        );
        assert_eq!(hook_words(&args(&["dibs", "claim", "hook"])), None);
    }

    #[test]
    fn a_hook_never_exits_2() {
        assert_eq!(exit_status(2), 0);
        assert_eq!(exit_status(0), 0);
        assert_eq!(exit_status(1), 1);
    }
}
