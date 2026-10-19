//! Repository chores, run as `cargo xtask <task>`:
//!
//! - `integrations` renders `docs/agent-workflow.md` into every file the integrations declare;
//! - `integrations --check` renders in memory and fails, naming the files, when one differs.

mod integrations;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "usage: cargo xtask integrations [--check]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = repository_root();
    if args.first().map(String::as_str) == Some("integrations")
        && !root.join(integrations::INTEGRATIONS).is_dir()
    {
        println!(
            "There is no {}/ directory yet; nothing to render.",
            integrations::INTEGRATIONS
        );
        return ExitCode::SUCCESS;
    }
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["integrations"] => write(&root),
        ["integrations", "--check"] => check(&root),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// The repository's root: the directory above this crate's.
fn repository_root() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .map_or_else(|| manifest_dir.to_path_buf(), Path::to_path_buf)
}

fn write(root: &Path) -> ExitCode {
    match integrations::write(root) {
        Ok(changed) if changed.is_empty() => {
            println!("Every integration's workflow text is up to date.");
            ExitCode::SUCCESS
        }
        Ok(changed) => {
            for path in changed {
                println!("rendered {}", path.display());
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("xtask integrations: {error}");
            ExitCode::FAILURE
        }
    }
}

fn check(root: &Path) -> ExitCode {
    match integrations::stale(root) {
        Ok(stale) if stale.is_empty() => {
            println!(
                "Every integration's workflow text matches {}.",
                integrations::SOURCE
            );
            ExitCode::SUCCESS
        }
        Ok(stale) => {
            eprintln!(
                "These files differ from what {} renders; run cargo xtask integrations:",
                integrations::SOURCE
            );
            for path in stale {
                eprintln!("  {}", path.display());
            }
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("xtask integrations: {error}");
            ExitCode::FAILURE
        }
    }
}
