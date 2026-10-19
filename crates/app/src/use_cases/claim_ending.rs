//! What every way of ending a claim does.

use time::OffsetDateTime;

use dibs_domain::claim::Claim;
use dibs_domain::event::EventKind;
use dibs_domain::item::ItemStatus;

use crate::error::AppError;
use crate::use_cases::common::remove_claim;
use crate::use_cases::grant_endings::end_orphaned_grants;
use crate::work::Work;

/// What ending a claim leaves behind: the item's new status and the event that records the end.
pub(crate) struct Ending {
    pub(crate) status: ItemStatus,
    pub(crate) event: EventKind,
}

/// Ends the claim inside the transaction: the claim and its shared paths go, the item takes its new
/// status, the end is recorded, and the grants that rested on the claim end with it.
pub(crate) async fn end_claim(
    work: &mut Work,
    claim: &Claim,
    ending: Ending,
    at: OffsetDateTime,
) -> Result<(), AppError> {
    let project = work.project_id();
    remove_claim(work, claim).await?;
    work.set_item_status(project, claim.item, ending.status)
        .await?;
    work.record(at, ending.event).await?;
    end_orphaned_grants(work, at).await
}
