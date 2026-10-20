//! Taking an area from debt work: the requests that ask a debt claim's holder to hand areas over,
//! the handover itself, the waits that end without one, the holder's narrowing that lets an area
//! go, and the sweep that hands over from claims gone stale and escalates the requests nobody
//! answered.
//!
//! What is due is decided by the domain from the claims as they stand in the transaction; this
//! module stores it, records it and tells the members it concerns, so whichever use case ended or
//! changed a claim settles what waited on it in its own transaction. One feature claim's waits on
//! one debt claim are asked in one conversation, which later requests of the same pair reuse.

mod narrow;
mod tick;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};

use time::OffsetDateTime;

use dibs_domain::area::QualifiedArea;
use dibs_domain::claim::{Claim, ClaimRefused, TakeoverRefused};
use dibs_domain::escalation::{EscalationReason, EscalationSubject, Parties};
use dibs_domain::event::{
    AreaPassing, EventKind, UnescalatedReason, WaitWithdrawn, YieldUnescalated,
};
use dibs_domain::handover::{
    AreaMove, WithdrawReason, Withdrawal, YieldPair, escalations_due, settle_pending,
};
use dibs_domain::ids::{ItemId, MemberId, MessageId, ThreadId};
use dibs_domain::message::{Author, Importance, Recipients};
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_domain::settings::{ProjectSettings, format_duration};

use crate::error::AppError;
use crate::named::{ItemName, Names, refuse};
use crate::ports::{IdGen, StoreError};
use crate::use_cases::common::existing_item;
use crate::use_cases::escalate::{Case, item_about};
use crate::use_cases::grant_endings::end_orphaned_grants;
use crate::use_cases::wide_asks::Announcer;
use crate::use_cases::writing::{Posted, SystemPost, SystemReply, system_body};
use crate::work::Work;

pub use narrow::{HandedOver, NarrowClaim, NarrowClaimInput, NarrowedClaim};
pub use tick::{HandoverSweep, TickHandovers};

/// What settling the pending areas did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Settled {
    /// The areas handed over.
    pub(crate) handed_over: Vec<AreaMove>,
    /// The requests sent, one per pair of claims.
    pub(crate) asked: Vec<YieldPair>,
    /// The waits that ended without a handover.
    pub(crate) withdrawn: Vec<Withdrawal>,
}

/// What one pass of escalations did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Escalated {
    /// Requests escalated, or added to an escalation of the same item that stands.
    pub(crate) escalated: usize,
    /// Areas whose request could not be escalated, since no arbiter would receive it.
    pub(crate) unescalated: usize,
}

/// What settling pending areas needs besides the transaction.
pub(crate) struct Handovers<'a> {
    pub(crate) ids: &'a dyn IdGen,
    pub(crate) schemes: &'a RefSchemeRegistry,
    pub(crate) settings: &'a ProjectSettings,
    pub(crate) now: OffsetDateTime,
}

impl Handovers<'_> {
    /// Settles every pending area of the project's claims as they stand in the transaction now:
    /// each area no other member holds any more moves to the claim waiting for it, recorded with
    /// `area_handed_over` and told to that claim's holder; each wait that can no longer be
    /// answered ends, recorded with `wait_withdrawn` and told to the debt holder it asked; and each
    /// debt holder working on areas a live claim waits for and not asked yet is asked, in the
    /// conversation already open for that pair of claims or a new one, recorded with
    /// `yield_requested`. Run it after any change that can end a claim, narrow it, make it wait
    /// for an area, or move it into or out of review.
    pub(crate) async fn settle(&self, work: &mut Work) -> Result<Settled, AppError> {
        let project = work.project_id();
        let claims = work.claims(project).await?;
        let settlement = settle_pending(&claims, self.now, self.settings.stale_after());
        if settlement.is_empty() {
            return Ok(Settled::default());
        }
        let names = Names::load(work).await?;
        let mut changed: BTreeMap<ItemId, Claim> = settlement
            .changed
            .into_iter()
            .map(|claim| (claim.item, claim))
            .collect();
        for pair in &settlement.asked {
            for moved in pair.moves() {
                work.record(self.now, EventKind::YieldRequested(passing(&moved)))
                    .await?;
            }
            let Some(thread) = self.ask(work, &names, &claims, pair).await? else {
                continue;
            };
            if let Some(waiting) = changed.get_mut(&pair.to) {
                waiting.note_request(pair.from, thread);
            }
        }
        for claim in changed.values() {
            work.save_claim(project, claim).await?;
        }
        for withdrawal in &settlement.withdrawn {
            self.withdraw(work, &names, &claims, withdrawal).await?;
        }
        for handed in &settlement.handed_over {
            work.record(self.now, EventKind::AreaHandedOver(passing(handed)))
                .await?;
            self.tell_handed_over(work, handed).await?;
        }
        if !settlement.handed_over.is_empty() {
            end_orphaned_grants(work, self.now).await?;
            Announcer {
                ids: self.ids,
                schemes: self.schemes,
                settings: self.settings,
                now: self.now,
            }
            .announce_due(work)
            .await?;
        }
        Ok(Settled {
            handed_over: settlement.handed_over,
            asked: settlement.asked,
            withdrawn: settlement.withdrawn,
        })
    }

    /// Escalates to the project's arbiters every request to hand areas over that waited the
    /// project's `yield_escalates_after` without them being let go, once each, one escalation per
    /// pair of claims: an urgent message, sent for the member whose claim waits, to the arbiters
    /// and the debt claim's holder, about the debt claim's item. When an escalation about that
    /// item already stands, the request is added to its conversation instead. When no arbiter
    /// would receive it, each of its areas is recorded with `yield_unescalated` and counted, so
    /// that the service warns; the request is marked escalated all the same, so that the sweep does
    /// not try it again every time it runs.
    pub(crate) async fn escalate_due(&self, work: &mut Work) -> Result<Escalated, AppError> {
        let project = work.project_id();
        let claims = work.claims(project).await?;
        let (changed, due) = escalations_due(
            &claims,
            self.now,
            self.settings.stale_after(),
            self.settings.yield_escalates_after(),
        );
        for claim in &changed {
            work.save_claim(project, claim).await?;
        }
        let names = Names::load(work).await?;
        let mut done = Escalated::default();
        for pair in &due {
            let (Some(waiting), Some(holding)) =
                (member_of(&claims, pair.to), member_of(&claims, pair.from))
            else {
                continue;
            };
            let from = existing_item(work, pair.from).await?;
            let to = self.name(work, pair.to).await?;
            let text = format!(
                "{feature}'s feature claim on {to} waits for {areas}, which {debt}'s debt claim on {from} holds. dibs asked {debt} to hand {them} over {after} ago, and {verb} not been handed over.",
                feature = names.handle(waiting)?,
                to = label(&to),
                areas = area_list(&pair.areas),
                debt = names.handle(holding)?,
                from = label(&ItemName::of(&from, self.schemes)),
                them = them(&pair.areas),
                after = format_duration(self.settings.yield_escalates_after()),
                verb = if pair.areas.len() == 1 {
                    "it has"
                } else {
                    "they have"
                },
            );
            let reason = EscalationReason::parse(&text).map_err(|_| unwritable())?;
            let case = Case {
                subject: EscalationSubject::Item(pair.from),
                about: item_about(&from, self.schemes),
                parties: Parties {
                    holders: BTreeSet::from([holding]),
                    requester: Some(waiting),
                },
                by: waiting,
                reason: &reason,
            };
            match case
                .send(work, self.ids, self.settings.arbiters(), self.now)
                .await
            {
                Ok(_) => done.escalated += 1,
                Err(AppError::AlreadyEscalated { thread }) => {
                    SystemReply {
                        thread,
                        message: MessageId(self.ids.next()),
                        body: system_body(&format!("Also escalated here: {text}"), None)?,
                        importance: Importance::Urgent,
                        ack_required: false,
                    }
                    .send(work, self.now)
                    .await?;
                    done.escalated += 1;
                }
                Err(AppError::Escalation(_)) => {
                    for moved in pair.moves() {
                        work.record(
                            self.now,
                            EventKind::YieldUnescalated(YieldUnescalated {
                                area: moved.area,
                                from: moved.from,
                                to: moved.to,
                                reason: UnescalatedReason::NoArbiters,
                            }),
                        )
                        .await?;
                        done.unescalated += 1;
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(done)
    }

    /// Asks the debt claim's holder to hand the pair's areas over: an urgent message that asks to
    /// be acknowledged, added to the conversation already asking for this pair when it still
    /// exists, else in a new conversation about the debt claim's item that the waiting claim's
    /// holder starts. Answers that conversation.
    async fn ask(
        &self,
        work: &mut Work,
        names: &Names,
        claims: &[Claim],
        pair: &YieldPair,
    ) -> Result<Option<ThreadId>, AppError> {
        let project = work.project_id();
        let (Some(waiting), Some(holding)) =
            (member_of(claims, pair.to), member_of(claims, pair.from))
        else {
            return Ok(None);
        };
        let to_item = existing_item(work, pair.to).await?;
        let to = ItemName::of(&to_item, self.schemes);
        let from = self.name(work, pair.from).await?;
        let feature = names.handle(waiting)?;
        let areas = area_list(&pair.areas);
        let flags: Vec<String> = pair
            .areas
            .iter()
            .map(|area| format!("--area {area}"))
            .collect();
        let lead = format!(
            "{feature} needs {areas} for work item {to_label} \"{title}\" (priority {priority}), and your debt claim on {from_label} holds {it}. Feature work takes an area from debt work. Tell your person now, and once they agree: commit what you have in {areas}, push your branch, give {it} up with dibs claim narrow {from_id} {flags} (MCP: dibs_narrow), and list in this conversation the files you hand over. From then on {feature}'s claim owns those files and yours drops them. If {it_is} not handed over within {after}, dibs takes it to the project's arbiters.",
            to_label = label(&to),
            title = to.title,
            priority = to_item.priority,
            from_label = label(&from),
            it = them(&pair.areas),
            from_id = from.id,
            flags = flags.join(" "),
            it_is = if pair.areas.len() == 1 {
                "it is"
            } else {
                "they are"
            },
            after = format_duration(self.settings.yield_escalates_after()),
        );
        let body = system_body(&lead, None)?;
        if let Some(thread) = pair.request
            && work.thread(project, thread).await?.is_some()
        {
            SystemReply {
                thread,
                message: MessageId(self.ids.next()),
                body,
                importance: Importance::Urgent,
                ack_required: true,
            }
            .send(work, self.now)
            .await?;
            return Ok(Some(thread));
        }
        let posted = SystemPost {
            ids: Posted::next(self.ids),
            subject: yield_subject(&pair.areas, &label(&to), to_item.priority),
            item: Some(pair.from),
            on_behalf_of: Author::Member(waiting),
            to: Recipients::Members([holding].into()),
            body,
            importance: Importance::Urgent,
            ack_required: true,
        }
        .send(work, self.now)
        .await?;
        Ok(Some(posted.thread))
    }

    /// Records a wait that ended without a handover and, when the debt holder was asked for it in
    /// a conversation that still exists, tells them there that the request no longer stands.
    pub(crate) async fn withdraw(
        &self,
        work: &mut Work,
        names: &Names,
        claims: &[Claim],
        withdrawal: &Withdrawal,
    ) -> Result<(), AppError> {
        let project = work.project_id();
        let moved = &withdrawal.moved;
        work.record(
            self.now,
            EventKind::WaitWithdrawn(WaitWithdrawn {
                area: moved.area.clone(),
                from: moved.from,
                to: moved.to,
                reason: withdrawal.reason,
            }),
        )
        .await?;
        let Some(thread) = withdrawal.request else {
            return Ok(());
        };
        if work.thread(project, thread).await?.is_none() {
            return Ok(());
        }
        let to = self.name(work, moved.to).await?;
        let waiter = match member_of(claims, moved.to) {
            Some(member) => names.handle(member)?.to_string(),
            None => "its holder".to_owned(),
        };
        let why = match withdrawal.reason {
            WithdrawReason::Stale => format!("{waiter}'s claim on {} went stale", label(&to)),
            WithdrawReason::Narrowed => {
                format!("{waiter} gave it up on their claim on {}", label(&to))
            }
            WithdrawReason::HolderChanged => {
                "another member's claim that a feature does not take from holds it now".to_owned()
            }
            WithdrawReason::Ended => format!("{waiter}'s claim on {} ended", label(&to)),
        };
        let lead = format!(
            "The request for {area} no longer stands: {why}. Nothing more is asked of you for it.",
            area = moved.area,
        );
        SystemReply {
            thread,
            message: MessageId(self.ids.next()),
            body: system_body(&lead, None)?,
            importance: Importance::Normal,
            ack_required: false,
        }
        .send(work, self.now)
        .await
    }

    /// Tells the holder of the claim an area was handed over to that it is theirs now.
    async fn tell_handed_over(&self, work: &mut Work, handed: &AreaMove) -> Result<(), AppError> {
        let project = work.project_id();
        let Some(receiving) = work.claim(project, handed.to).await? else {
            return Ok(());
        };
        let to = self.name(work, handed.to).await?;
        let from = self.name(work, handed.from).await?;
        let area = &handed.area;
        let lead = format!(
            "{area} is yours now: no other member's claim holds it any more, so it moved from the debt claim on {from_label} to your claim on work item {to_label} \"{title}\". Rebase onto the base before you edit there, and read the conversation that asked for it for the files handed over.",
            from_label = label(&from),
            to_label = label(&to),
            title = to.title,
        );
        SystemPost {
            ids: Posted::next(self.ids),
            subject: format!("Handed over: {area}"),
            item: Some(handed.to),
            on_behalf_of: Author::System,
            to: Recipients::Members([receiving.member].into()),
            body: system_body(&lead, None)?,
            importance: Importance::High,
            ack_required: false,
        }
        .send(work, self.now)
        .await?;
        Ok(())
    }

    async fn name(&self, work: &mut Work, item: ItemId) -> Result<ItemName, AppError> {
        Ok(ItemName::of(
            &existing_item(work, item).await?,
            self.schemes,
        ))
    }
}

/// The subject of a request to hand areas over: `Yield <areas> to <item>, priority <n>`.
pub(crate) fn yield_subject(
    areas: &BTreeSet<QualifiedArea>,
    to: &str,
    priority: dibs_domain::item::Priority,
) -> String {
    format!("Yield {} to {to}, priority {priority}", area_list(areas))
}

/// What another claim's wait in the way of saving `claim` is, when saving it broke the rule that
/// an area waits for one claim at a time: the area, the claim that waits for it and whether that
/// claim is of `by` too.
async fn waited_elsewhere(
    work: &mut Work,
    claim: &Claim,
    by: MemberId,
) -> Result<Option<(QualifiedArea, MemberId, ItemId, bool)>, AppError> {
    let project = work.project_id();
    let claims = work.claims(project).await?;
    Ok(claims
        .iter()
        .filter(|other| other.item != claim.item)
        .find_map(|other| {
            claim
                .pending
                .keys()
                .find(|area| other.waits_for(area))
                .map(|area| (area.clone(), other.member, other.item, other.member == by))
        }))
}

/// Stores a claim a member asked for. Should the store find that one of its areas waits already
/// for another claim — which the decision that made it rules out — the member is refused as the
/// decision would have refused them, never answered with a failure of the service.
pub(crate) async fn save_claimed(work: &mut Work, claim: &Claim) -> Result<(), AppError> {
    let project = work.project_id();
    match work.save_claim(project, claim).await {
        Err(StoreError::Duplicate) => match waited_elsewhere(work, claim, claim.member).await? {
            Some((area, member, item, own)) => Err(refuse(
                work,
                ClaimRefused::AreaWaited {
                    area,
                    member,
                    item,
                    own,
                },
            )
            .await),
            None => Err(AppError::Store(StoreError::Duplicate)),
        },
        other => Ok(other?),
    }
}

/// [`save_claimed`] for a takeover.
pub(crate) async fn save_taken_over(work: &mut Work, claim: &Claim) -> Result<(), AppError> {
    let project = work.project_id();
    match work.save_claim(project, claim).await {
        Err(StoreError::Duplicate) => match waited_elsewhere(work, claim, claim.member).await? {
            Some((area, member, item, own)) => Err(refuse(
                work,
                TakeoverRefused::AreaWaited {
                    area,
                    member,
                    item,
                    own,
                },
            )
            .await),
            None => Err(AppError::Store(StoreError::Duplicate)),
        },
        other => Ok(other?),
    }
}

/// The member of the claim on `item`, when there is one.
fn member_of(claims: &[Claim], item: ItemId) -> Option<MemberId> {
    claims
        .iter()
        .find(|claim| claim.item == item)
        .map(|claim| claim.member)
}

/// The event payload of an area on its way.
fn passing(moved: &AreaMove) -> AreaPassing {
    AreaPassing {
        area: moved.area.clone(),
        from: moved.from,
        to: moved.to,
    }
}

/// Areas as a sentence lists them: `api:billing`, `api:billing and api:search`.
fn area_list(areas: &BTreeSet<QualifiedArea>) -> String {
    let names: Vec<String> = areas.iter().map(ToString::to_string).collect();
    match names.as_slice() {
        [] => "the area".to_owned(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// `it` for one area, `them` for several.
fn them(areas: &BTreeSet<QualifiedArea>) -> &'static str {
    if areas.len() == 1 { "it" } else { "them" }
}

/// How an item reads in what dibs writes: its reference, or `#<id>`.
fn label(item: &ItemName) -> String {
    item.external_ref
        .as_ref()
        .map_or_else(|| format!("#{}", item.id), ToString::to_string)
}

fn unwritable() -> AppError {
    AppError::Store(StoreError::Corrupt {
        what: "the reason of an escalation dibs writes is not valid".to_owned(),
    })
}
