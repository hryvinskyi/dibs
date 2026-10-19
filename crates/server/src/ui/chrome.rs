//! What every signed-in page shows around its content: the project, who is signed in, how many
//! messages wait unread, the page's place in the navigation, and the form token of the sign-out
//! form.

use dibs_app::use_cases::InboxQuery;
use dibs_domain::settings::ProjectSettings;

use crate::error::ApiError;
use crate::http::AppState;
use crate::ui::assets::BUNDLE_HASH;
use crate::ui::session::UiSession;

/// The page's place in the navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    /// The status page.
    Status,
    /// The board.
    Board,
    /// The threads and a thread.
    Threads,
    /// A page outside the navigation.
    Other,
}

impl Section {
    /// The name the templates compare against.
    const fn name(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Board => "board",
            Self::Threads => "threads",
            Self::Other => "",
        }
    }
}

/// The frame of a signed-in page.
#[derive(Debug, Clone)]
pub struct Chrome {
    /// The bundle directory the page loads its stylesheet and script from.
    pub assets: &'static str,
    /// The project's slug.
    pub project: String,
    /// The handle of the member signed in.
    pub handle: String,
    /// How many messages the member has not read.
    pub unread: u64,
    /// The session's form token.
    pub csrf: String,
    /// The navigation entry of the page: `status`, `board`, `threads` or empty.
    pub section: &'static str,
}

/// The frame of a page and the project's settings, read for the session's member.
#[derive(Debug, Clone)]
pub struct Signed {
    /// The frame.
    pub chrome: Chrome,
    /// The project's settings.
    pub settings: ProjectSettings,
}

/// Reads what the frame of a page shows.
///
/// # Errors
/// The answer for a use case's failure.
pub async fn load(
    state: &AppState,
    session: &UiSession,
    section: Section,
) -> Result<Signed, ApiError> {
    let actor = session.actor();
    let me = state.app.me.execute(actor).await?;
    let unread = state
        .app
        .inbox
        .execute(
            actor,
            InboxQuery {
                unread_only: true,
                unacked: false,
            },
        )
        .await?
        .threads
        .iter()
        .map(|summary| summary.unread)
        .sum();
    Ok(Signed {
        chrome: Chrome {
            assets: BUNDLE_HASH,
            project: me.project.slug.to_string(),
            handle: me.member.handle.to_string(),
            unread,
            csrf: session.form_token(),
            section: section.name(),
        },
        settings: me.settings,
    })
}
