//! `dibs check`: the verdict on paths about to change, or warnings about the paths a range of
//! commits changed.
//!
//! Both work inside a checkout only: the nearest `.dibs.toml` names the repository, which must be
//! the checkout's `origin`. A path no area or hot-file rule governs is answered without the server.
//! With paths, a path that may not be edited makes the command fail as refused. With a range, every
//! problem is a warning and the command succeeds, so a git hook calling it never stops a push.

use std::path::PathBuf;

use dibs_client::api::{EditCheck, EditCheckRequest, PathVerdict, Verdict};
use dibs_client::checkout::Checkout;
use dibs_client::verdicts::edit_verdicts;
use dibs_domain::paths::RelPath;
use dibs_domain::repo::RepoId;
use dibs_domain::settings::ProjectSettings;

use crate::cli::CheckArgs;
use crate::context::{Connection, Context};
use crate::failure::{Failure, sentence};
use crate::git::Git;
use crate::output::verdict_line;

/// Runs `dibs check`.
///
/// # Errors
/// With paths, a [`Failure`]: refused when a path may not be edited now. With a range, none.
pub async fn check(args: CheckArgs, ctx: &Context) -> Result<(), Failure> {
    match args.git_range {
        Some(range) => {
            range_warnings(&range, args.author.as_deref(), ctx).await;
            Ok(())
        }
        None => paths(args.paths, ctx).await,
    }
}

/// A connection with the checkout it runs in, its repository confirmed against `origin`.
struct Governed {
    connection: Connection,
    checkout: Checkout,
    settings: ProjectSettings,
    repo: RepoId,
}

async fn governed(ctx: &Context) -> Result<Governed, Failure> {
    let connection = ctx.connect().await?;
    let Some(checkout) = connection.checkout.clone() else {
        return Err(Failure::config(
            "no_checkout",
            "No .dibs.toml was found here or above; run dibs check inside a checkout.",
        ));
    };
    let settings = connection.me.project_settings().map_err(|error| {
        Failure::config(
            "unknown_settings",
            sentence(&format!(
                "the project's settings use something this dibs does not know ({error}); update dibs"
            )),
        )
    })?;
    let origin = Git::new(checkout.root()).origin();
    let repo = checkout
        .governing_repo(&settings, origin.as_ref())
        .map_err(|refused| Failure::config("wrong_checkout", sentence(&refused.to_string())))?
        .id
        .clone();
    Ok(Governed {
        connection,
        checkout,
        settings,
        repo,
    })
}

async fn verdicts(governed: &Governed, paths: Vec<RelPath>) -> Result<Vec<PathVerdict>, Failure> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let connection = &governed.connection;
    let session = connection.session().await?;
    let request = EditCheckRequest {
        session_id: session,
        repo: governed.repo.clone(),
        paths,
        reserve: false,
    };
    edit_verdicts(&connection.client, &governed.settings, &request)
        .await
        .map_err(|error| connection.failed(error))
}

async fn paths(given: Vec<PathBuf>, ctx: &Context) -> Result<(), Failure> {
    let governed = governed(ctx).await?;
    let paths = given
        .iter()
        .map(|path| {
            governed
                .checkout
                .relative_path(&ctx.cwd, path)
                .map_err(|refused| Failure::config("invalid_path", sentence(&refused.to_string())))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let verdicts = verdicts(&governed, paths).await?;
    let blocked = verdicts.iter().filter(|v| !v.verdict.permits()).count();
    let answer = EditCheck { verdicts };
    ctx.printer.show(&answer, || {
        answer
            .verdicts
            .iter()
            .map(verdict_line)
            .collect::<Vec<_>>()
            .join("\n")
    });
    if blocked > 0 {
        return Err(Failure::refused(
            "not_allowed",
            format!(
                "{blocked} of {} paths may not be edited now.",
                answer.verdicts.len()
            ),
        )
        .after_answer());
    }
    Ok(())
}

async fn range_warnings(range: &str, author: Option<&str>, ctx: &Context) {
    match range_verdicts(range, author, ctx).await {
        Ok(verdicts) => {
            if !ctx.printer.is_json() {
                for verdict in verdicts.iter().filter(|v| warns(&v.verdict)) {
                    ctx.printer.warn(&verdict_line(verdict));
                }
            }
            ctx.printer.show(&EditCheck { verdicts }, String::new);
        }
        Err(failure) => {
            let text = format!(
                "the commits of {range} were not checked: {}",
                failure.message
            );
            ctx.printer.warn(&text);
            ctx.printer.show(
                &serde_json::json!({"verdicts": [], "warning": text}),
                String::new,
            );
        }
    }
}

/// Whether a range check warns about a verdict: anything but a plain or granted permission.
fn warns(verdict: &Verdict) -> bool {
    !matches!(verdict, Verdict::Allowed | Verdict::Granted { .. })
}

async fn range_verdicts(
    range: &str,
    author: Option<&str>,
    ctx: &Context,
) -> Result<Vec<PathVerdict>, Failure> {
    let governed = governed(ctx).await?;
    let changed = Git::new(governed.checkout.root()).changed_paths(range, author)?;
    let paths: Vec<RelPath> = changed
        .iter()
        .filter_map(|path| RelPath::parse(path).ok())
        .collect();
    verdicts(&governed, paths).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_warns_about_everything_but_a_permission() {
        assert!(!warns(&Verdict::Allowed));
        assert!(warns(&Verdict::NeedsClaim));
    }
}
