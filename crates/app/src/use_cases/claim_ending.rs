//! What every way of ending a claim does, and how its holder is told.

use time::OffsetDateTime;

use dibs_domain::claim::Claim;
use dibs_domain::event::EventKind;
use dibs_domain::handover::{AreaMove, WithdrawReason, Withdrawal};
use dibs_domain::item::ItemStatus;
use dibs_domain::message::{Author, Importance, Recipients};
use dibs_domain::ref_scheme::RefSchemeRegistry;

use crate::error::AppError;
use crate::named::{ItemName, Names};
use crate::ports::IdGen;
use crate::use_cases::common::{existing_item, remove_claim};
use crate::use_cases::grant_endings::end_orphaned_grants;
use crate::use_cases::handover::Handovers;
use crate::use_cases::writing::{Posted, SystemPost, titled_body};
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

/// Tells the claim's holder, in a message from dibs filed under the claim's item, what became of
/// their claim: the subject is `subject` followed by the item's title, and `text` writes the
/// message from how the item is named (its reference, or `#` and its id) and its title. The
/// message reaches the holder alone, at normal importance, and asks no acknowledgement.
pub(crate) async fn tell_holder(
    work: &mut Work,
    ids: &dyn IdGen,
    schemes: &RefSchemeRegistry,
    claim: &Claim,
    subject: &str,
    text: impl FnOnce(&str, &str) -> String,
    now: OffsetDateTime,
) -> Result<(), AppError> {
    let item = ItemName::of(&existing_item(work, claim.item).await?, schemes);
    let named = item
        .external_ref
        .as_ref()
        .map_or_else(|| format!("#{}", item.id), ToString::to_string);
    SystemPost {
        ids: Posted::next(ids),
        subject: format!("{subject}{}", item.title),
        item: Some(item.id),
        on_behalf_of: Author::System,
        to: Recipients::Members([claim.member].into()),
        body: titled_body(&text(&named, item.title.as_str()), None)?,
        importance: Importance::Normal,
        ack_required: false,
    }
    .send(work, now)
    .await?;
    Ok(())
}
