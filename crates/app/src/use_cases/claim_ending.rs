//! What every way of ending a claim does.

use dibs_domain::claim::Claim;
use dibs_domain::event::EventKind;
use dibs_domain::handover::{AreaMove, WithdrawReason, Withdrawal};
use dibs_domain::item::ItemStatus;

use crate::error::AppError;
use crate::named::Names;
use crate::use_cases::common::remove_claim;
use crate::use_cases::grant_endings::end_orphaned_grants;
use crate::use_cases::handover::Handovers;
use crate::work::Work;

/// What ending a claim leaves behind: the item's new status and the event that records the end.
pub(crate) struct Ending {
    pub(crate) status: ItemStatus,
    pub(crate) event: EventKind,
}

/// Ends the claim inside the transaction: the waits it had end (`wait_withdrawn`, the debt holders
/// asked told so), the claim, its shared paths and its footprints go, the item takes its new status,
/// the end is recorded, the grants that rested on the claim end with it, and what other claims
/// waited for it to hand over moves to them.
pub(crate) async fn end_claim(
    work: &mut Work,
    claim: &Claim,
    ending: Ending,
    handovers: &Handovers<'_>,
) -> Result<(), AppError> {
    let project = work.project_id();
    let at = handovers.now;
    if !claim.pending.is_empty() {
        let claims = work.claims(project).await?;
        let names = Names::load(work).await?;
        for (area, pending) in &claim.pending {
            let withdrawal = Withdrawal {
                moved: AreaMove {
                    area: area.clone(),
                    from: pending.from,
                    to: claim.item,
                },
                reason: WithdrawReason::Ended,
                request: pending.request,
            };
            handovers
                .withdraw(work, &names, &claims, &withdrawal)
                .await?;
        }
    }
    remove_claim(work, claim).await?;
    work.set_item_status(project, claim.item, ending.status)
        .await?;
    work.record(at, ending.event).await?;
    end_orphaned_grants(work, at).await?;
    handovers.settle(work).await?;
    Ok(())
}
