//! What dibs does with a request for a grant on the holder's behalf, wherever it decides one: it
//! judges the request from what it stores, and when it grants, it gives the holder's grant,
//! settles the request and says so in the request's conversation, which both members read.

use time::OffsetDateTime;

use dibs_domain::area::QualifiedArea;
use dibs_domain::claim::Claim;
use dibs_domain::grant::{
    self, AskedGrant, GrantBlock, GrantDecision, GrantFacts, OnBehalf, decide_on_holders_behalf,
};
use dibs_domain::handle::Handle;
use dibs_domain::ids::{GrantId, MessageId, ThreadId};
use dibs_domain::message::Importance;
use dibs_domain::radar::Footprint;
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_domain::settings::ProjectSettings;

use crate::error::AppError;
use crate::named::{ItemName, NamedGrant};
use crate::ports::IdGen;
use crate::records::GrantRecord;
use crate::use_cases::claim_footprints::ClaimFootprints;
use crate::use_cases::common::existing_item;
use crate::use_cases::grant::{GiveGrant, Giving};
use crate::use_cases::writing::{SystemReply, titled_body};
use crate::work::Work;

/// What dibs reads, inside one transaction of one project, to decide requests on their holders'
/// behalf, and what it writes with.
pub(crate) struct OnBehalfOf<'a> {
    /// The project's claims.
    pub(crate) claims: &'a [Claim],
    /// The revokes that stick.
    pub(crate) blocks: &'a [GrantBlock],
    /// The project's footprints, listed without their paths.
    pub(crate) listed: &'a [Footprint],
    /// The project's settings.
    pub(crate) settings: &'a ProjectSettings,
    /// Where new ids come from.
    pub(crate) ids: &'a dyn IdGen,
    /// How items are named.
    pub(crate) schemes: &'a RefSchemeRegistry,
    /// When.
    pub(crate) now: OffsetDateTime,
}

impl OnBehalfOf<'_> {
    /// What dibs does with `asked`, asked at `asked_at`, among `records`, the project's grants as
    /// the transaction sees them (see [`decide_on_holders_behalf`]). A revoke that sticks against
    /// the request is looked for first: when one stands, no footprint is read. Otherwise the
    /// footprints of the holder's claim are read, with their paths, only while the request waits
    /// for that holder and the claim is not in review, the one reason that needs none.
    pub(crate) async fn decide(
        &self,
        work: &mut Work,
        asked: &AskedGrant,
        asked_at: OffsetDateTime,
        records: &[GrantRecord],
    ) -> Result<OnBehalf, AppError> {
        let grants: Vec<grant::Grant> = records.iter().map(|r| r.grant.clone()).collect();
        let blocked = self
            .blocks
            .iter()
            .any(|block| block.applies_to(asked, self.claims));
        let holder_claim = asked
            .waits_on(&grants, self.claims, self.now, self.settings)
            .and_then(|item| {
                self.claims
                    .iter()
                    .find(|claim| claim.member == asked.holder && claim.item == item)
            });
        let stored = match holder_claim {
            Some(claim) if !blocked && !claim.state.is_in_review() => {
                ClaimFootprints::read(work, claim.item, self.listed).await?
            }
            Some(_) | None => ClaimFootprints::default(),
        };
        let footprints = stored.paths();
        let facts = GrantFacts {
            grants: &grants,
            claims: self.claims,
            footprints: &footprints,
            blocks: self.blocks,
        };
        Ok(decide_on_holders_behalf(
            asked,
            asked_at,
            &facts,
            self.settings,
            self.now,
        ))
    }

    /// Whether `asked`, asked at `asked_at`, has closed among `records`, the project's grants as
    /// the transaction sees them (see [`decide_on_holders_behalf`]): the claim it was asked under
    /// has ended, or its holder holds the area through no claim. Closing is decided before
    /// anything else, a revoke that sticks included, so no footprint is read for it. It is all
    /// dibs judges of a request left to its holder.
    pub(crate) fn closed(
        &self,
        asked: &AskedGrant,
        asked_at: OffsetDateTime,
        records: &[GrantRecord],
    ) -> bool {
        let grants: Vec<grant::Grant> = records.iter().map(|r| r.grant.clone()).collect();
        let facts = GrantFacts {
            grants: &grants,
            claims: self.claims,
            footprints: &[],
            blocks: self.blocks,
        };
        decide_on_holders_behalf(asked, asked_at, &facts, self.settings, self.now)
            == OnBehalf::Closed
    }

    /// Grants `asked`, asked at `asked_at`, on its holder's behalf because of `because`, as the
    /// holder's grant, given as every grant is given (see [`GiveGrant`]) and recorded with when the
    /// oldest request it answers was asked, this one or an earlier one it settles; settles the
    /// request, granted or not, so that dibs never decides it again; and, when the grant is new,
    /// says so and why in `thread`, the request's conversation. Answers the grant given, or `None`
    /// when the same grant existed or the rules of a grant refused it.
    pub(crate) async fn grant(
        &self,
        work: &mut Work,
        asked: &AskedGrant,
        asked_at: OffsetDateTime,
        because: GrantDecision,
        thread: ThreadId,
        records: &[GrantRecord],
    ) -> Result<Option<NamedGrant>, AppError> {
        let project = work.project_id();
        let item = ItemName::of(&existing_item(work, asked.ask.item).await?, self.schemes);
        let giving = GiveGrant {
            request: grant::Grant {
                item: asked.ask.item,
                area: asked.ask.area.clone(),
                from: asked.holder,
                to: asked.ask.asker,
            },
            decided: because,
            asked_at: Some(asked_at),
            item: &item,
            records,
            claims: self.claims,
            settings: self.settings,
            ids: self.ids,
            now: self.now,
        };
        let given = match giving.give(work).await? {
            Giving::Given(grant) => Some(grant),
            Giving::Existing(_) | Giving::Refused(_) => None,
        };
        work.settle_asked_grant(project, asked.message, self.now)
            .await?;
        if let (Some(grant), Some(why)) = (&given, because.why()) {
            let text = DecidedText {
                asker: &grant.to,
                holder: &grant.from,
                item: &item,
                area: &grant.record.grant.area,
                why,
                grant: grant.record.id,
            };
            SystemReply {
                thread,
                message: MessageId(self.ids.next()),
                body: titled_body(&text.lead(), None)?,
                importance: Importance::Normal,
                ack_required: false,
            }
            .send(work, self.now)
            .await?;
        }
        Ok(given)
    }
}

/// What dibs writes in a request's conversation when it grants the request on the holder's behalf.
struct DecidedText<'a> {
    /// The member who asked, now the grantee.
    asker: &'a Handle,
    /// The holder whose grant it is.
    holder: &'a Handle,
    /// The asker's item, the grant's item.
    item: &'a ItemName,
    /// The area.
    area: &'a QualifiedArea,
    /// Why dibs granted it, as [`GrantDecision::why`] says it.
    why: &'a str,
    /// The grant given.
    grant: GrantId,
}

impl DecidedText<'_> {
    /// Who may now edit where, for which item, on whose behalf and why; that the order rule still
    /// decides which change lands first where both meet; and who may undo it, and how.
    fn lead(&self) -> String {
        format!(
            "Granted by dibs: {asker} may edit {area} for work item {label} \"{title}\", on {holder}'s behalf, because {why}. Where both changes meet in a file, the order rule decides which lands first. To undo it, {holder} or an admin revokes grant {grant} (dibs revoke-grant {grant}, or DELETE /v1/grants/{grant}).",
            asker = self.asker,
            area = self.area,
            label = self.item.label(),
            title = self.item.title,
            holder = self.holder,
            why = self.why,
            grant = self.grant,
        )
    }
}

#[cfg(test)]
mod tests {
    use dibs_domain::ids::ItemId;
    use dibs_domain::item::{ExternalRef, Title};

    use super::*;

    #[test]
    fn the_reply_names_the_grant_why_and_how_to_undo_it() {
        let item = ItemName {
            id: ItemId(2),
            external_ref: Some(ExternalRef::parse("gh:acme/widgets#2").unwrap()),
            title: Title::parse("Refund rounding").unwrap(),
            link: None,
        };
        let text = DecidedText {
            asker: &Handle::parse("bob").unwrap(),
            holder: &Handle::parse("alice").unwrap(),
            item: &item,
            area: &QualifiedArea::parse("api:billing").unwrap(),
            why: GrantDecision::InReview.why().unwrap(),
            grant: GrantId(7),
        };
        assert_eq!(
            text.lead(),
            "Granted by dibs: bob may edit api:billing for work item gh:acme/widgets#2 \"Refund rounding\", on alice's behalf, because the claim holding the area is in review. Where both changes meet in a file, the order rule decides which lands first. To undo it, alice or an admin revokes grant 7 (dibs revoke-grant 7, or DELETE /v1/grants/7)."
        );
    }
}
