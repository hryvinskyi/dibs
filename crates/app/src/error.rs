//! Why a use case refused or failed, each cause with a stable code.

use thiserror::Error;

use dibs_domain::claim::{ClaimRefused, ForceReleaseRefused, HolderOnly, TakeoverRefused};
use dibs_domain::delivery::DeliveryRefused;
use dibs_domain::dependency::DependencyRefused;
use dibs_domain::edit::EditCheckRefused;
use dibs_domain::escalation::EscalationRefused;
use dibs_domain::grant::{GrantRefused, RevokeRefused};
use dibs_domain::handle::Handle;
use dibs_domain::ids::{EventId, GrantId, ItemId, MessageId, SessionId, ThreadId};
use dibs_domain::import::BatchRefused;
use dibs_domain::item::{CreateRefused, ItemStatus};
use dibs_domain::message::{AckRefused, ImportanceLimited, RecipientsError};
use dibs_domain::settings::{AreaLookupError, SettingsError};
use dibs_domain::slot::{BeatRefused, SlotRefused};
use dibs_domain::wide::WideScopeError;

use crate::named::Refusal;
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
    /// No such message, or one in a thread the caller may not read.
    #[error("there is no message {0}")]
    Message(MessageId),
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
    /// No project of that slug.
    #[error("there is no project '{0}'")]
    Project(ProjectSlug),
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
    /// Only the author of a message may see whom it reached.
    #[error("only the author of a message can see who read and acknowledged it")]
    NotAuthor,
    /// The acknowledgement was refused.
    #[error(transparent)]
    Ack(#[from] AckRefused),
    /// A delivery of a message or an offer was refused.
    #[error(transparent)]
    Delivery(#[from] DeliveryRefused),
    /// The member wrote as many high or urgent messages within the hour as the project allows.
    #[error(
        "{0}; send this one as normal, or send it as {importance} after {frees}",
        importance = "high or urgent",
        frees = utc_clock(.0.frees_at)
    )]
    ImportanceLimited(#[from] ImportanceLimited),
    /// The claim was refused; the refusal carries the handle of the member in the way.
    #[error(transparent)]
    Claim(#[from] Refusal<ClaimRefused>),
    /// The takeover was refused; the refusal carries the handle of the member in the way.
    #[error(transparent)]
    Takeover(#[from] Refusal<TakeoverRefused>),
    /// The release or completion was refused; the refusal carries the holder's handle.
    #[error(transparent)]
    HolderOnly(#[from] Refusal<HolderOnly>),
    /// The forced release was refused.
    #[error(transparent)]
    ForceRelease(#[from] ForceReleaseRefused),
    /// A slot request was refused; the refusal carries the handle of the member in the way.
    #[error(transparent)]
    Slot(#[from] Refusal<SlotRefused>),
    /// A run's heartbeat found its hold gone; the refusal carries the handle of who holds it now.
    #[error(transparent)]
    Beat(#[from] Refusal<BeatRefused>),
    /// The grant was refused.
    #[error(transparent)]
    Grant(#[from] GrantRefused),
    /// The revocation was refused.
    #[error(transparent)]
    Revoke(#[from] RevokeRefused),
    /// The item may not be created.
    #[error(transparent)]
    Create(#[from] CreateRefused),
    /// The dependency may not be added.
    #[error(transparent)]
    Dependency(#[from] DependencyRefused),
    /// The import batch is refused as a whole.
    #[error(transparent)]
    Batch(#[from] BatchRefused),
    /// An area named in the request does not resolve.
    #[error(transparent)]
    Area(#[from] AreaLookupError),
    /// The wide scope asked for cannot be made.
    #[error(transparent)]
    Wide(#[from] WideScopeError),
    /// The settings document is invalid.
    #[error(transparent)]
    Settings(#[from] SettingsError),
    /// The edit check cannot be answered.
    #[error(transparent)]
    EditCheck(#[from] EditCheckRefused),
    /// The message names no recipient.
    #[error(transparent)]
    Recipients(#[from] RecipientsError),
    /// The escalation was refused.
    #[error(transparent)]
    Escalation(#[from] EscalationRefused),
    /// The subject has an escalation that no arbiter has settled yet.
    #[error(
        "this is escalated already, in conversation {thread}; follow the arbiters' answer there instead of escalating again"
    )]
    AlreadyEscalated {
        /// The conversation of the escalation that stands.
        thread: ThreadId,
    },
    /// An admin asked to remove themself.
    #[error("you cannot remove yourself from the project; another admin, or the operator, can")]
    SelfRemoval,
    /// A reader's cursor is behind events the retention removed: what it missed cannot be read
    /// any more.
    #[error(
        "events after this cursor were removed by the retention; read the current state again, then follow the log from event {through}"
    )]
    CursorExpired {
        /// The newest event removed.
        through: EventId,
    },
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
            Self::NotAuthor => "not_author",
            Self::Ack(refused) => match refused {
                AckRefused::NotRequired => "ack_not_required",
                AckRefused::NotRecipient => "not_a_recipient",
            },
            Self::Delivery(refused) => match refused {
                DeliveryRefused::NotRecipient => "not_a_recipient",
                DeliveryRefused::OfferGone => "offer_gone",
                DeliveryRefused::Delivered { .. } => "delivered",
            },
            Self::ImportanceLimited(_) => "importance_limited",
            Self::Claim(refused) => match refused.reason {
                ClaimRefused::HeldBy { .. } => "held",
                ClaimRefused::Stale { .. } => "stale",
                ClaimRefused::AreaHeld { .. } => "area_held",
                ClaimRefused::UnknownArea { .. } => "unknown_area",
                ClaimRefused::WideOverlap { .. } => "wide_overlap",
                ClaimRefused::WideAndHeld { .. } => "wide_and_held",
            },
            Self::Takeover(refused) => match refused.reason {
                TakeoverRefused::NothingToTake => "nothing_to_take",
                TakeoverRefused::OwnClaim => "own_claim",
                TakeoverRefused::NotStale { .. } => "not_stale",
                TakeoverRefused::AreaHeld { .. } => "area_held",
                TakeoverRefused::UnknownArea { .. } => "unknown_area",
                TakeoverRefused::WideOverlap { .. } => "wide_overlap",
                TakeoverRefused::WideAndHeld { .. } => "wide_and_held",
            },
            Self::HolderOnly(refused) => match refused.reason {
                HolderOnly::NoClaim => "no_claim",
                HolderOnly::NotHolder { .. } => "not_holder",
            },
            Self::ForceRelease(refused) => match refused {
                ForceReleaseRefused::NotAdmin => "forbidden",
                ForceReleaseRefused::NoClaim => "no_claim",
            },
            Self::Slot(refused) => match refused.reason {
                SlotRefused::UnknownSlot { .. } => "unknown_slot",
                SlotRefused::HoldsOther { .. } => "holds_other_slot",
                SlotRefused::NotHeld => "slot_not_held",
                SlotRefused::NotHolder { .. } => "not_holder",
                SlotRefused::RunEnded => "run_ended",
                SlotRefused::NotWaiting => "not_waiting",
                SlotRefused::OwnHold => "own_hold",
                SlotRefused::NotStalled { .. } => "not_stalled",
            },
            Self::Beat(refused) => match refused.reason {
                BeatRefused::TakenOver { .. } => "taken_over",
                BeatRefused::Ended => "run_ended",
            },
            Self::Grant(refused) => match refused {
                GrantRefused::SelfGrant => "self_grant",
                GrantRefused::UnknownArea { .. } => "unknown_area",
                GrantRefused::NotHolder { .. } => "not_holder",
                GrantRefused::GranteeHasNoClaim => "grantee_has_no_claim",
            },
            Self::Revoke(RevokeRefused::NotGrantor) => "not_grantor",
            Self::Create(CreateRefused::UnlistedRepository { .. }) => "unlisted_repository",
            Self::Dependency(refused) => match refused {
                DependencyRefused::SelfDependency => "self_dependency",
                DependencyRefused::Cycle => "dependency_cycle",
            },
            Self::Batch(refused) => match refused {
                BatchRefused::TooMany { .. } => "batch_too_large",
                BatchRefused::CompleteWithoutSource => "invalid_batch",
                BatchRefused::Duplicate { .. } => "duplicate_ref",
            },
            Self::Wide(WideScopeError::NoAreas) => "invalid_wide",
            Self::Area(refused) => match refused {
                AreaLookupError::Malformed { .. } => "invalid_area",
                AreaLookupError::NotFound { .. } => "unknown_area",
                AreaLookupError::Ambiguous { .. } => "ambiguous_area",
            },
            Self::Settings(_) => "invalid_settings",
            Self::EditCheck(EditCheckRefused::UnknownRepo { .. }) => "unknown_repo",
            Self::Recipients(RecipientsError::Empty) => "no_recipients",
            Self::Escalation(EscalationRefused::NoArbiters) => "no_arbiters",
            Self::AlreadyEscalated { .. } => "already_escalated",
            Self::SelfRemoval => "self_removal",
            Self::CursorExpired { .. } => "cursor_expired",
            Self::StoredSettings(_) | Self::Store(_) => "internal",
        }
    }
}

/// A time as a sentence names it: `14:05 UTC`.
fn utc_clock(at: time::OffsetDateTime) -> String {
    let at = at.to_offset(time::UtcOffset::UTC);
    format!("{:02}:{:02} UTC", at.hour(), at.minute())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_importance_limit_says_when_it_frees_up() {
        let frees_at = time::OffsetDateTime::UNIX_EPOCH
            + time::Duration::hours(10)
            + time::Duration::seconds(42 * 60 + 7);
        let error = AppError::from(ImportanceLimited {
            limit: 10,
            frees_at,
        });
        assert_eq!(error.code(), "importance_limited");
        assert_eq!(
            error.to_string(),
            "you sent 10 high or urgent messages within the last hour, as many as this project allows; send this one as normal, or send it as high or urgent after 10:42 UTC"
        );
    }

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
