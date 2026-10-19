//! Ending, inside a transaction, the grants a change of claims left without a footing.

use time::OffsetDateTime;

use dibs_domain::event::{EventKind, GrantEnd, GrantRevoked};
use dibs_domain::grant::orphaned;

use crate::error::AppError;
use crate::records::GrantRecord;
use crate::work::Work;

/// The event that records a grant's end.
pub(crate) fn grant_revoked(record: &GrantRecord, cause: GrantEnd) -> EventKind {
    EventKind::GrantRevoked(GrantRevoked {
        grant: record.id,
        item: record.grant.item,
        area: record.grant.area.clone(),
        from: record.grant.from,
        to: record.grant.to,
        cause,
    })
}

/// Removes every grant the claims as they now stand no longer carry, and records each end.
pub(crate) async fn end_orphaned_grants(
    work: &mut Work,
    at: OffsetDateTime,
) -> Result<(), AppError> {
    let project = work.project_id();
    let claims = work.claims(project).await?;
    let grants = work.grants(project).await?;
    for (record, cause) in orphaned(&grants, &claims) {
        work.delete_grant(project, record.id).await?;
        work.record(at, grant_revoked(record, cause)).await?;
    }
    Ok(())
}
