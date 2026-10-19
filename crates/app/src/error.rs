//! Why a use case refused or failed, each cause with a stable code.

use thiserror::Error;

use dibs_domain::claim::{ClaimRefused, ForceReleaseRefused, HolderOnly, TakeoverRefused};
use dibs_domain::edit::EditCheckRefused;
use dibs_domain::grant::{GrantRefused, RevokeRefused};
use dibs_domain::handle::Handle;
use dibs_domain::ids::{GrantId, ItemId, SessionId, ThreadId};
use dibs_domain::item::{CreateRefused, ItemStatus};
use dibs_domain::message::RecipientsError;
use dibs_domain::settings::{AreaLookupError, SettingsError};

use crate::ports::StoreError;
use crate::token::TokenPrefix;
use dibs_domain::project::ProjectSlug;

/// What a request named that does not exist in the caller's project. A record of another project is
/// reported the same way, so the answer never confirms that it exists.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Missing {
    /// No such work item.
    #[error("there is no work item {0}")]
    Item(ItemId),
    /// No such thread, or one the caller may not read.
    #[error("there is no thread {0}")]
    Thread(ThreadId),
    /// No such grant.
    #[error("there is no grant {0}")]
    Grant(GrantId),
    /// No member of that handle.
    #[error("there is no member '{0}'")]
    Member(Handle),
    /// No session of that id belonging to the caller.
    #[error("there is no session {0} of yours; register the session first")]
    Session(SessionId),
    /// No token of that prefix.
    #[error("there is no token starting with {0}")]
    Token(TokenPrefix),
}

/// A record that would duplicate one that exists.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Duplicate {
    /// A project of that slug exists.
    #[error("a project '{0}' already exists")]
    Project(ProjectSlug),
    /// A member of that handle exists in the project.
    #[error("the handle '{0}' is taken in this project")]
    Handle(Handle),
}

/// Why a use case refused or failed. [`AppError::code`] names each cause in a stable form; the
/// `Display` text is a sentence for the person who asked.
#[derive(Debug, Error)]
pub enum AppError {
    /// The token or browser session is unknown, malformed, revoked or expired. Every cause reads
    /// the same.
    #[error("the token is not valid; sign in again with a current token")]
    Unauthorized,
    /// The caller's role does not allow this.
    #[error("your role in this project does not allow this")]
    Forbidden,
    /// The request names something the project does not have.
    #[error(transparent)]
    NotFound(Missing),
    /// The request would duplicate a record.
    #[error(transparent)]
    Duplicate(Duplicate),
    /// The item is done or dropped.
    #[error("this work item is closed; it can no longer be claimed")]
    ItemClosed {
        /// Its status.
        status: ItemStatus,
    },
    /// A recipient is an integration, which receives no message.
    #[error("'{0}' is an integration and receives no messages")]
    NotARecipient(Handle),
    /// The claim was refused.
    #[error(transparent)]
    Claim(#[from] ClaimRefused),
    /// The takeover was refused.
    #[error(transparent)]
    Takeover(#[from] TakeoverRefused),
    /// The release or completion was refused.
    #[error(transparent)]
    HolderOnly(#[from] HolderOnly),
    /// The forced release was refused.
    #[error(transparent)]
    ForceRelease(#[from] ForceReleaseRefused),
    /// The grant was refused.
    #[error(transparent)]
    Grant(#[from] GrantRefused),
    /// The revocation was refused.
    #[error(transparent)]
    Revoke(#[from] RevokeRefused),
    /// The item may not be created.
    #[error(transparent)]
    Create(#[from] CreateRefused),
    /// An area named in the request does not resolve.
    #[error(transparent)]
    Area(#[from] AreaLookupError),
    /// The settings document is invalid.
    #[error(transparent)]
    Settings(#[from] SettingsError),
    /// The edit check cannot be answered.
    #[error(transparent)]
    EditCheck(#[from] EditCheckRefused),
    /// The message names no recipient.
    #[error(transparent)]
    Recipients(#[from] RecipientsError),
    /// The stored settings no longer parse with the rule kinds this deployment knows.
    #[error("the service could not complete the request; an admin needs to check the settings")]
    StoredSettings(#[source] SettingsError),
    /// The store failed.
    #[error("the service could not complete the request; try again")]
    Store(#[from] StoreError),
}

impl AppError {
    /// The cause in a stable `snake_case` form, for clients to act on.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::NotFound(_) => "not_found",
            Self::Duplicate(Duplicate::Project(_)) => "project_exists",
            Self::Duplicate(Duplicate::Handle(_)) => "handle_taken",
            Self::ItemClosed { .. } => "item_closed",
            Self::NotARecipient(_) => "not_a_recipient",
            Self::Claim(refused) => match refused {
                ClaimRefused::HeldBy { .. } => "held",
                ClaimRefused::Stale { .. } => "stale",
                ClaimRefused::AreaHeld { .. } => "area_held",
                ClaimRefused::UnknownArea { .. } => "unknown_area",
            },
            Self::Takeover(refused) => match refused {
                TakeoverRefused::NothingToTake => "nothing_to_take",
                TakeoverRefused::OwnClaim => "own_claim",
                TakeoverRefused::NotStale { .. } => "not_stale",
                TakeoverRefused::AreaHeld { .. } => "area_held",
                TakeoverRefused::UnknownArea { .. } => "unknown_area",
            },
            Self::HolderOnly(refused) => match refused {
                HolderOnly::NoClaim => "no_claim",
                HolderOnly::NotHolder { .. } => "not_holder",
            },
            Self::ForceRelease(refused) => match refused {
                ForceReleaseRefused::NotAdmin => "forbidden",
                ForceReleaseRefused::NoClaim => "no_claim",
            },
            Self::Grant(refused) => match refused {
                GrantRefused::SelfGrant => "self_grant",
                GrantRefused::UnknownArea { .. } => "unknown_area",
                GrantRefused::NotHolder { .. } => "not_holder",
                GrantRefused::GranteeHasNoClaim => "grantee_has_no_claim",
            },
            Self::Revoke(RevokeRefused::NotGrantor) => "not_grantor",
            Self::Create(CreateRefused::UnlistedRepository { .. }) => "unlisted_repository",
            Self::Area(refused) => match refused {
                AreaLookupError::Malformed { .. } => "invalid_area",
                AreaLookupError::NotFound { .. } => "unknown_area",
                AreaLookupError::Ambiguous { .. } => "ambiguous_area",
            },
            Self::Settings(_) => "invalid_settings",
            Self::EditCheck(EditCheckRefused::UnknownRepo { .. }) => "unknown_repo",
            Self::Recipients(RecipientsError::Empty) => "no_recipients",
            Self::StoredSettings(_) | Self::Store(_) => "internal",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_store_failure_tells_nothing_of_its_cause() {
        let error = AppError::Store(StoreError::Corrupt {
            what: "claims row 7: areas column".to_owned(),
        });
        assert_eq!(error.code(), "internal");
        assert!(!error.to_string().contains("claims row"));
    }

    #[test]
    fn unauthorized_reads_the_same_for_every_cause() {
        assert_eq!(AppError::Unauthorized.code(), "unauthorized");
        assert_eq!(
            AppError::Unauthorized.to_string(),
            "the token is not valid; sign in again with a current token"
        );
    }
}
