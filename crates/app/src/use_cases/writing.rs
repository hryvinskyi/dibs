//! Storing a message inside the caller's transaction, held to the project's limit on the high and
//! urgent messages a member writes.

use time::OffsetDateTime;

use dibs_domain::message::{RAISED_WINDOW, Writer, counts_toward_limit, decide_raised};

use crate::error::AppError;
use crate::records::MessageRecord;
use crate::work::Work;

/// Who writes a message, and what limit holds them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Authorship {
    /// The message's author wrote it, held to this many high or urgent messages an hour.
    Member {
        /// The project's limit.
        limit: u32,
    },
    /// dibs writes it when something happens.
    Dibs,
}

/// Stores `message`. A message its member wrote at a raised importance is refused when they already
/// wrote as many within the hour as the project allows, and is otherwise recorded toward that
/// limit; whether a message counts at all is the domain's rule.
pub(crate) async fn write_message(
    work: &mut Work,
    message: &MessageRecord,
    authorship: Authorship,
    now: OffsetDateTime,
) -> Result<(), AppError> {
    let project = work.project_id();
    let (writer, limit) = match authorship {
        Authorship::Member { limit } => (Writer::Member, limit),
        Authorship::Dibs => (Writer::Dibs, 0),
    };
    let counted = if counts_toward_limit(writer, message.importance) {
        let written = work
            .raised_since(project, message.author, now - RAISED_WINDOW)
            .await?;
        decide_raised(writer, message.importance, &written, limit, now)?
    } else {
        false
    };
    work.insert_message(project, message).await?;
    if counted {
        work.insert_raised(project, message.author, message.id, now)
            .await?;
    }
    Ok(())
}
