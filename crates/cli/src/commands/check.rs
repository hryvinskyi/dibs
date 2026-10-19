//! `dibs check`: the verdict on paths about to change, or warnings about the paths a range of
//! commits changed.
//!
//! Both work inside a checkout only: the nearest `.dibs.toml` names the repository, which must be
//! the checkout's `origin`. A path no area or hot-file rule governs is answered without the server.
//! With paths, a path that may not be edited makes the command fail as refused. With a range, every
//! problem is a warning and the command succeeds, so a git hook calling it never stops a push.

use std::path::{Path, PathBuf};

use dibs_client::api::{EditCheck, EditCheckRequest, Me, PathVerdict, Verdict};
use dibs_client::checkout::Checkout;
use dibs_client::verdicts::edit_verdicts;
use dibs_domain::ids::SessionId;
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

/// The checkout a command runs in, its repository confirmed against `origin`, and the project's
/// settings as they stood when it was asked.
pub struct Governed {
    checkout: Checkout,
    settings: ProjectSettings,
    repo: RepoId,
}

impl Governed {
    /// The checkout of `connection`, governed by the settings `me` carries.
    ///
    /// # Errors
    /// A configuration [`Failure`] when the connection has no checkout, the settings use
    /// something this dibs does not know, or the checkout is not a clone of a listed repository.
    pub fn of(connection: &Connection, me: &Me) -> Result<Self, Failure> {
        let Some(checkout) = connection.checkout.clone() else {
            return Err(Failure::config(
                "no_checkout",
                "No .dibs.toml was found here or above in this repository; run dibs check inside a checkout.",
            ));
        };
        let settings = me.project_settings().map_err(|error| {
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
        Ok(Self {
            checkout,
            settings,
            repo,
        })
    }

    /// The path as the checkout names it, whether it was given relative to `cwd` or absolute.
    ///
    /// # Errors
    /// A configuration [`Failure`] when the path lies outside the checkout.
    pub fn relative(&self, cwd: &Path, path: &Path) -> Result<RelPath, Failure> {
        self.checkout
            .relative_path(cwd, path)
            .map_err(|refused| Failure::config("invalid_path", sentence(&refused.to_string())))
    }

    /// The verdicts on `paths`, in the order asked, for a session's edits; nothing is reserved.
    /// A path no rule governs is answered without the server.
    ///
    /// # Errors
    /// The [`Failure`] of the call.
    pub async fn verdicts(
        &self,
        connection: &Connection,
        session: SessionId,
        paths: Vec<RelPath>,
    ) -> Result<Vec<PathVerdict>, Failure> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let request = EditCheckRequest {
            session_id: session,
            repo: self.repo.clone(),
            paths,
            reserve: false,
        };
        edit_verdicts(&connection.client, &self.settings, &request)
            .await
            .map_err(|error| connection.failed(error))
    }
}

async fn governed(ctx: &Context) -> Result<(Connection, Governed), Failure> {
    let connection = ctx.connect().await?;
    let governed = Governed::of(&connection, &connection.me)?;
    Ok((connection, governed))
}

async fn verdicts(
    connection: &Connection,
    governed: &Governed,
    paths: Vec<RelPath>,
) -> Result<Vec<PathVerdict>, Failure> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let session = connection.session().await?;
    governed.verdicts(connection, session, paths).await
}

async fn paths(given: Vec<PathBuf>, ctx: &Context) -> Result<(), Failure> {
    let (connection, governed) = governed(ctx).await?;
    let paths = given
        .iter()
        .map(|path| governed.relative(&ctx.cwd, path))
        .collect::<Result<Vec<_>, _>>()?;
    let verdicts = verdicts(&connection, &governed, paths).await?;
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
    let (connection, governed) = governed(ctx).await?;
    let changed = Git::new(governed.checkout.root()).changed_paths(range, author)?;
    let paths: Vec<RelPath> = changed
        .iter()
        .filter_map(|path| RelPath::parse(path).ok())
        .collect();
    verdicts(&connection, &governed, paths).await
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
