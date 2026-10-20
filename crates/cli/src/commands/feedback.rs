//! `dibs feedback`: telling the project's admins that a refusal of dibs was wrong, or that a rebase
//! conflict was resolved by hand. A refusal is named by the id its reason ended with; its kind is
//! sent too while this machine still remembers it. A conflict is named by its file, which the
//! server keeps only as the area it lies in. The reason is kept as written for 90 days, read by the
//! member and the project's admins; `-` reads it from standard input.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use time::OffsetDateTime;

use dibs_client::api::{FeedbackAbout, FeedbackGiven, FeedbackRequest};
use dibs_domain::report::{Feedback, FeedbackReason, RefusalId};

use crate::cli::FeedbackCommand;
use crate::commands::check::Governed;
use crate::context::{Connection, Context};
use crate::failure::Failure;
use crate::guard::report::refusal_tally;

/// What the feedback is about, as the member names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedbackOn {
    /// A refusal, by its id.
    FalseRefusal(RefusalId),
    /// A file, relative to the current directory or absolute.
    Conflict(PathBuf),
}

/// Sends the feedback over `connection`, a conflict's file read from `cwd`.
///
/// # Errors
/// A configuration [`Failure`] for a conflict outside the checkout; the [`Failure`] of the call.
pub async fn give(
    connection: &Connection,
    cwd: &Path,
    on: FeedbackOn,
    reason: FeedbackReason,
) -> Result<FeedbackGiven, Failure> {
    let about = match on {
        FeedbackOn::FalseRefusal(refusal) => FeedbackAbout::FalseRefusal {
            tally: refusal_tally(&connection.state, &refusal, OffsetDateTime::now_utc()),
            refusal,
        },
        FeedbackOn::Conflict(path) => {
            let governed = Governed::of(connection, &connection.me)?;
            FeedbackAbout::Conflict {
                path: governed.relative(cwd, &path)?,
                repo: governed.repo().clone(),
            }
        }
    };
    connection
        .client
        .feedback(&FeedbackRequest { about, reason })
        .await
        .map_err(|error| connection.failed(error))
}

/// Runs `dibs feedback`.
///
/// # Errors
/// The [`Failure`] of [`give`].
pub async fn feedback(command: FeedbackCommand, ctx: &Context) -> Result<(), Failure> {
    let (on, reason) = match command {
        FeedbackCommand::FalseRefusal(args) => (FeedbackOn::FalseRefusal(args.id), args.reason),
        FeedbackCommand::Conflict(args) => (FeedbackOn::Conflict(args.path), args.reason),
    };
    let reason = reason_of(&reason)?;
    let connection = ctx.connect().await?;
    let given = give(&connection, &ctx.cwd, on, reason).await?;
    ctx.printer.show(&given, || said(&given.feedback));
    Ok(())
}

/// The reason as given, or read from standard input for `-`.
///
/// # Errors
/// A configuration [`Failure`] when it cannot be read or is not one line of 1 to 280 characters.
fn reason_of(given: &str) -> Result<FeedbackReason, Failure> {
    let text = if given == "-" {
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text).map_err(|_| {
            Failure::config(
                "no_reason",
                "The reason could not be read from standard input.",
            )
        })?;
        text
    } else {
        given.to_owned()
    };
    FeedbackReason::parse(&text).map_err(|_| {
        Failure::config(
            "invalid_reason",
            "A reason is one line of 1 to 280 characters; name no path, command line or secret in it.",
        )
    })
}

/// What the command line says once feedback was recorded.
pub fn said(feedback: &Feedback) -> String {
    match feedback {
        Feedback::FalseRefusal { refusal, tally } => {
            let kind = tally.map_or_else(String::new, |tally| format!(" ({tally})"));
            format!("Recorded for the project's admins: the refusal {refusal}{kind} was wrong.")
        }
        Feedback::Conflict { repo, area } => match area {
            Some(area) => {
                format!("Recorded for the project's admins: a conflict resolved by hand in {area}.")
            }
            None => format!(
                "Recorded for the project's admins: a conflict resolved by hand in {repo}, outside every area."
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use dibs_domain::area::QualifiedArea;
    use dibs_domain::repo::RepoId;
    use dibs_domain::report::Tally;

    use super::*;

    #[test]
    fn what_was_recorded_reads_as_its_kind() {
        let refusal = Feedback::FalseRefusal {
            refusal: RefusalId::parse("r-7f3a").unwrap(),
            tally: Some(Tally::HeldBy),
        };
        assert_eq!(
            said(&refusal),
            "Recorded for the project's admins: the refusal r-7f3a (held_by) was wrong."
        );
        let conflict = Feedback::Conflict {
            repo: RepoId::implicit(),
            area: Some(QualifiedArea::parse("default:billing").unwrap()),
        };
        assert!(said(&conflict).ends_with("in default:billing."));
        let outside = Feedback::Conflict {
            repo: RepoId::implicit(),
            area: None,
        };
        assert!(said(&outside).ends_with("outside every area."));
    }
}
