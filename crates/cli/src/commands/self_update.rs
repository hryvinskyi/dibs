//! `dibs self-update`: replaces this binary with a newer release, verified against the release key
//! compiled into it, and the same update run in the background by `dibs sync` when the project's
//! server recommends a newer client.
//!
//! One update runs at a time on a machine: each holds the update lock in the state directory for
//! its whole run, and one that cannot take it leaves the work to the one that holds it.

use std::fs::{File, OpenOptions, TryLockError};

use semver::Version;
use serde::Serialize;

use dibs_client::locations::Locations;
use dibs_client::update::{UpdateError, UpdateRequest, Updated, Wanted, releases, update};

use crate::cli::SelfUpdateArgs;
use crate::context::Context;
use crate::failure::{Failure, Kind};
use crate::release;

/// The name of the lock file an update holds.
const UPDATE_LOCK: &str = "update.lock";

/// What `dibs self-update` prints with `--json`.
#[derive(Serialize)]
struct Answer {
    updated: bool,
    from: String,
    to: String,
}

/// Runs `dibs self-update`.
///
/// # Errors
/// A [`Failure`] saying why nothing was installed.
pub async fn self_update(args: SelfUpdateArgs, ctx: &Context) -> Result<(), Failure> {
    let wanted = match args.version.as_deref() {
        None => Wanted::Latest,
        Some(text) => Wanted::Exactly(parse_version(text)?),
    };
    let updated = install(wanted, ctx).await?;
    let answer = match &updated {
        Updated::Current(version) => Answer {
            updated: false,
            from: version.to_string(),
            to: version.to_string(),
        },
        Updated::Installed { from, to } => Answer {
            updated: true,
            from: from.to_string(),
            to: to.to_string(),
        },
    };
    ctx.printer.show(&answer, || match updated {
        Updated::Current(version) => format!("dibs {version} is installed; nothing to update."),
        Updated::Installed { from, to } => format!("Updated dibs from {from} to {to}."),
    });
    Ok(())
}

/// Installs the release `wanted`, unless another update is running on this machine.
///
/// # Errors
/// A [`Failure`] saying why nothing was installed.
pub async fn install(wanted: Wanted, ctx: &Context) -> Result<Updated, Failure> {
    let key = release::trusted_key();
    if !key.is_set() {
        return Err(refusal(&UpdateError::NoKey));
    }
    let installed = release::installed()?;
    let binary = release::running_binary()?;
    let source = releases(&release::releases_url(ctx.env())).map_err(|error| refusal(&error))?;
    let _lock = take_lock(&ctx.locations()?)?;
    let request = UpdateRequest {
        installed,
        platform: release::platform(),
        wanted,
        binary,
    };
    update(&key, source.as_ref(), &request)
        .await
        .map_err(|error| refusal(&error))
}

/// A version written as `1.2.3` or `v1.2.3`.
fn parse_version(text: &str) -> Result<Version, Failure> {
    let text = text.trim();
    Version::parse(text.strip_prefix('v').unwrap_or(text)).map_err(|_| {
        Failure::config(
            "invalid_version",
            format!("'{text}' is not a release version; write one such as 1.2.3."),
        )
    })
}

/// The machine's update lock, taken; a refusal when another update holds it.
fn take_lock(locations: &Locations) -> Result<File, Failure> {
    let failed = |error: std::io::Error| {
        tracing::debug!(error = %error, "the update lock could not be taken");
        Failure::config(
            "state",
            "The update lock could not be taken on this machine.",
        )
    };
    std::fs::create_dir_all(locations.state_dir()).map_err(failed)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(locations.state_dir().join(UPDATE_LOCK))
        .map_err(failed)?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(TryLockError::WouldBlock) => Err(Failure::refused(
            "update_running",
            "Another dibs update is running on this machine; it does the work.",
        )),
        Err(TryLockError::Error(error)) => Err(failed(error)),
    }
}

/// The failure of a refused update.
fn refusal(error: &UpdateError) -> Failure {
    let mut failure = Failure::refused(error.code(), crate::failure::sentence(&error.to_string()));
    if error.is_unavailable() {
        failure.kind = Kind::Unreachable;
    }
    failure
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_is_written_with_or_without_its_v() {
        assert_eq!(parse_version("v1.2.3").unwrap(), Version::new(1, 2, 3));
        assert_eq!(parse_version(" 0.4.0 ").unwrap(), Version::new(0, 4, 0));
        let refused = parse_version("latest").unwrap_err();
        assert_eq!(refused.kind, Kind::Config);
    }

    #[test]
    fn a_download_that_failed_is_unreachable_and_every_other_refusal_refused() {
        let download = refusal(&UpdateError::Download {
            asset: "SHA256SUMS".to_owned(),
        });
        assert_eq!(download.kind, Kind::Unreachable);
        let unsigned = refusal(&UpdateError::Unsigned);
        assert_eq!(
            (unsigned.kind, unsigned.code.as_str()),
            (Kind::Refused, "unsigned")
        );
        assert!(
            unsigned.message.starts_with("The release"),
            "{}",
            unsigned.message
        );
        assert!(unsigned.message.ends_with('.'), "{}", unsigned.message);
    }
}
