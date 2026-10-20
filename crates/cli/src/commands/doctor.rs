//! `dibs doctor`: what this machine has for the guard, read without the server — the agent hosts
//! this binary supports and what each can carry, what is installed for them, the checkout and
//! sign-in of the current directory, the switch, and how long hooks took today.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;
use time::OffsetDateTime;

use dibs_agent_host::{HostAdapter, Probe};
use dibs_client::checkout::Checkout;
use dibs_client::credentials::{CredentialKey, Credentials, OsKeychain, ReadMode, TokenLookup};
use dibs_client::locations::Locations;
use dibs_client::state::StateDir;

use crate::failure::{Failure, sentence};
use crate::guard::{OFF_VAR, switched_off, timing};
use crate::hosts;
use crate::output::Printer;

#[derive(Serialize)]
struct Report {
    version: &'static str,
    state_dir: Option<PathBuf>,
    switched_off: bool,
    checkout: Option<CheckoutReport>,
    hosts: Vec<HostReport>,
    hook_p95_ms_today: BTreeMap<String, Option<u64>>,
}

#[derive(Serialize)]
struct CheckoutReport {
    root: PathBuf,
    server: String,
    project: String,
    repo: Option<String>,
    signed_in: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    unreachable_since: Option<OffsetDateTime>,
}

#[derive(Serialize)]
struct HostReport {
    id: &'static str,
    capabilities: BTreeMap<&'static str, bool>,
    installed: Vec<ProbeReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    push: Option<PushReport>,
}

/// What pushing to a session that waits needs on a host that can do it.
#[derive(Serialize)]
struct PushReport {
    /// The variable that opts a session in.
    opt_in: &'static str,
    /// Whether this environment opts in.
    opted_in: bool,
    /// The variable through which the host names the session it started the tool server for.
    #[serde(skip_serializing_if = "Option::is_none")]
    session_var: Option<&'static str>,
}

#[derive(Serialize)]
struct ProbeReport {
    what: &'static str,
    found: bool,
}

/// Runs `dibs doctor`.
///
/// # Errors
/// A configuration [`Failure`] when the current directory cannot be read.
pub fn doctor(env: &HashMap<String, String>, printer: Printer) -> Result<(), Failure> {
    let cwd = std::env::current_dir()
        .map_err(|_| Failure::config("no_directory", "The current directory cannot be read."))?;
    let lookup = |name: &str| env.get(name).cloned();
    let locations = Locations::from_env(&lookup).ok();
    let checkout = Checkout::discover(&cwd)
        .map_err(|error| Failure::config("invalid_checkout", sentence(&error.to_string())))?;
    let registry = hosts::registry();
    let today = OffsetDateTime::now_utc().date();
    let report = Report {
        version: env!("CARGO_PKG_VERSION"),
        state_dir: locations.as_ref().map(|l| l.state_dir().to_path_buf()),
        switched_off: switched_off(env.get(OFF_VAR).map(String::as_str)),
        checkout: checkout
            .as_ref()
            .map(|checkout| checkout_report(checkout, locations.as_ref(), &lookup)),
        hosts: registry
            .adapters()
            .map(|adapter| host_report(adapter, env))
            .collect(),
        hook_p95_ms_today: locations
            .as_ref()
            .and_then(|locations| {
                timing::load(locations.state_dir()).day(today).map(|day| {
                    day.iter()
                        .map(|(key, buckets)| (key.clone(), buckets.p95_bound_ms()))
                        .collect()
                })
            })
            .unwrap_or_default(),
    };
    printer.show(&report, || text(&report));
    Ok(())
}

fn checkout_report(
    checkout: &Checkout,
    locations: Option<&Locations>,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> CheckoutReport {
    let key = CredentialKey {
        server: checkout.config().server.clone(),
        project: checkout.config().project.clone(),
    };
    let signed_in = locations.is_some_and(|locations| {
        Credentials::new(
            locations.credentials_file(),
            std::sync::Arc::new(OsKeychain),
            lookup,
        )
        .and_then(|credentials| credentials.token(&key, ReadMode::Hook))
        .is_ok_and(|found| matches!(found, TokenLookup::Found { .. }))
    });
    let unreachable_since = locations
        .and_then(|locations| StateDir::new(locations.state_dir(), &key).unreachable_since());
    CheckoutReport {
        root: checkout.root().to_path_buf(),
        server: key.server.to_string(),
        project: key.project.to_string(),
        repo: checkout.config().repo.as_ref().map(ToString::to_string),
        signed_in,
        unreachable_since,
    }
}

fn host_report(adapter: &dyn HostAdapter, env: &HashMap<String, String>) -> HostReport {
    HostReport {
        id: adapter.id().as_str(),
        capabilities: adapter.capabilities().named().into_iter().collect(),
        installed: adapter
            .probes()
            .iter()
            .map(|probe| probe_report(probe, env))
            .collect(),
        push: adapter.push_extension().map(|extension| PushReport {
            opt_in: crate::mcp::PUSH_VAR,
            opted_in: crate::mcp::opted_in(env.get(crate::mcp::PUSH_VAR).map(String::as_str)),
            session_var: extension.session_var(),
        }),
    }
}

fn probe_report(probe: &Probe, env: &HashMap<String, String>) -> ProbeReport {
    match probe {
        Probe::Program { name, meaning } => ProbeReport {
            what: meaning,
            found: on_path(name, env.get("PATH").map(String::as_str)),
        },
        Probe::HomePath { path, meaning } => ProbeReport {
            what: meaning,
            found: home(env).is_some_and(|home| home.join(path).exists()),
        },
    }
}

fn home(env: &HashMap<String, String>) -> Option<PathBuf> {
    ["HOME", "USERPROFILE"]
        .iter()
        .find_map(|name| env.get(*name).filter(|value| !value.is_empty()))
        .map(PathBuf::from)
}

/// Whether a program of this name is in one of the `PATH` directories.
fn on_path(name: &str, path: Option<&str>) -> bool {
    let Some(path) = path else {
        return false;
    };
    let names = [
        name.to_owned(),
        format!("{name}.exe"),
        format!("{name}.cmd"),
    ];
    std::env::split_paths(path).any(|dir| names.iter().any(|name| is_file(&dir.join(name))))
}

fn is_file(path: &Path) -> bool {
    path.is_file()
}

fn yes(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn text(report: &Report) -> String {
    let mut out = format!("dibs {}\n", report.version);
    if let Some(dir) = &report.state_dir {
        let _ = writeln!(out, "State directory: {}", dir.display());
    }
    let _ = writeln!(
        out,
        "Switched off ({OFF_VAR}): {}",
        yes(report.switched_off)
    );
    match &report.checkout {
        Some(checkout) => {
            let _ = writeln!(
                out,
                "Checkout: {} (project {} on {}{})",
                checkout.root.display(),
                checkout.project,
                checkout.server,
                checkout
                    .repo
                    .as_ref()
                    .map_or_else(String::new, |repo| format!(", repo {repo}"))
            );
            let _ = writeln!(out, "Signed in: {}", yes(checkout.signed_in));
            if let Some(since) = checkout.unreachable_since {
                let _ = writeln!(out, "Server unreachable since: {since}");
            }
        }
        None => out.push_str("Checkout: none governs this directory\n"),
    }
    out.push_str("Agent hosts:\n");
    for host in &report.hosts {
        let can: Vec<&str> = host
            .capabilities
            .iter()
            .filter(|(_, on)| **on)
            .map(|(name, _)| *name)
            .collect();
        let _ = writeln!(out, "  {}: carries {}", host.id, can.join(", "));
        if let Some(push) = &host.push {
            let _ = writeln!(
                out,
                "    pushes to a waiting session: {} ({}=1 for the session)",
                if push.opted_in {
                    "opted in"
                } else {
                    "not opted in"
                },
                push.opt_in
            );
        }
        if host.installed.is_empty() {
            let _ = writeln!(
                out,
                "    nothing to install: anything that runs dibs hook {} <event> uses it",
                host.id
            );
        }
        for probe in &host.installed {
            let _ = writeln!(
                out,
                "    {}: {}",
                probe.what,
                if probe.found { "found" } else { "not found" }
            );
        }
    }
    if report.hook_p95_ms_today.is_empty() {
        out.push_str("Hook runs today: none recorded\n");
    } else {
        out.push_str("Hook runs today, 95th percentile:\n");
        for (key, p95) in &report.hook_p95_ms_today {
            let bound =
                p95.map_or_else(|| "over 2000 ms".to_owned(), |ms| format!("within {ms} ms"));
            let _ = writeln!(out, "  {key}: {bound}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_is_found_in_a_path_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tool"), "").unwrap();
        let path = dir.path().to_string_lossy().into_owned();
        assert!(on_path("tool", Some(&path)));
        assert!(!on_path("other", Some(&path)));
        assert!(!on_path("tool", None));
    }
}
