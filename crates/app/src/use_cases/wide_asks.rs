//! Sending, inside the caller's transaction, the announcements wide claims owe the holders of their
//! areas, and turning an acknowledgement of one — the holder's, or dibs's on the holder's behalf —
//! into the holder's grants.
//!
//! An announcement is a thread about the wide claim's item, addressed to the one holder it asks,
//! whose single message is `high` and asks to be acknowledged. dibs writes it, under the session
//! label `dibs`, in a thread the wide claim's member starts: the member did not type it, its text
//! says dibs sent it for them, and the holder's answer is to them.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use time::OffsetDateTime;

use dibs_domain::area::QualifiedArea;
use dibs_domain::claim::{Claim, FootprintPaths};
use dibs_domain::event::{EventKind, WideGrantedByAck};
use dibs_domain::grant::{Grant, GrantBlock, GrantDecision};
use dibs_domain::handle::Handle;
use dibs_domain::ids::{GrantId, ItemId, MessageId, ThreadId};
use dibs_domain::message::{Author, Body, Importance, Recipients};
use dibs_domain::radar::Footprint;
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_domain::settings::ProjectSettings;
use dibs_domain::wide::{
    AckOnBehalf, AskDue, WideAckFacts, WideAsk, asks_due, decide_ack_on_holders_behalf,
    grants_by_ack, judged_areas,
};

use crate::error::AppError;
use crate::named::{ItemName, NamedGrant, Names};
use crate::ports::{IdGen, StoreError};
use crate::records::GrantRecord;
use crate::use_cases::claim_footprints::ClaimFootprints;
use crate::use_cases::common::existing_item;
use crate::use_cases::writing::{Posted, SystemPost, SystemReply, TITLE_NOTE, titled_body};
use crate::work::Work;

/// How many areas the text of an announcement lists by name before it counts the rest.
const LISTED_AREAS: usize = 20;

/// An announcement sent, with the handle of the holder it asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Announced {
    /// The announcement. Its `acked_by_dibs` says why dibs acknowledged it on the holder's behalf
    /// as it was sent, and is `None` while it waits for the holder.
    pub ask: WideAsk,
    /// The thread that carries it.
    pub thread: ThreadId,
    /// The handle of `ask.to`.
    pub to: Handle,
}

/// What sending announcements needs besides the transaction.
pub(crate) struct Announcer<'a> {
    pub(crate) ids: &'a dyn IdGen,
    pub(crate) schemes: &'a RefSchemeRegistry,
    pub(crate) settings: &'a ProjectSettings,
    pub(crate) now: OffsetDateTime,
}

impl Announcer<'_> {
    /// Sends every announcement the project's claims, as they stand in the transaction now, owe:
    /// one per holder of an area a live wide claim names that no announcement still waiting for
    /// dibs or the holder asked them about (see [`asks_due`]). One dibs settled because it closed
    /// asks for nothing any more, so a holder who gave its areas up and holds them again is asked
    /// anew. Each one sent is judged at once, in the same transaction, and acknowledged by
    /// dibs on its holder's behalf when the test of [`decide_ack_on_holders_behalf`] says so. Run it
    /// after any change that can make a member hold an area or widen a wide claim.
    pub(crate) async fn announce_due(&self, work: &mut Work) -> Result<Vec<Announced>, AppError> {
        let project = work.project_id();
        let claims = work.claims(project).await?;
        let grants: Vec<Grant> = work
            .grants(project)
            .await?
            .into_iter()
            .map(|record| record.grant)
            .collect();
        let asked = work.waiting_wide_asks(project).await?;
        let blocks = work.grant_blocks(project).await?;
        let due = asks_due(
            &claims,
            &grants,
            &asked,
            &blocks,
            self.now,
            self.settings.stale_after(),
        );
        if due.is_empty() {
            return Ok(Vec::new());
        }
        let names = Names::load(work).await?;
        let mut sent = Vec::with_capacity(due.len());
        for ask in due {
            let reason = claims
                .iter()
                .find(|claim| claim.item == ask.item)
                .and_then(|claim| claim.wide.as_ref())
                .map(|scope| scope.reason().as_str().to_owned())
                .unwrap_or_default();
            sent.push(self.send(work, &names, ask, &reason).await?);
        }
        let listed = work.footprints(project).await?;
        let acks = DibsAcks {
            ids: self.ids,
            schemes: self.schemes,
            settings: self.settings,
            now: self.now,
        };
        let facts = AckFacts {
            claims: &claims,
            blocks: &blocks,
            listed: &listed,
        };
        for announced in &mut sent {
            if let AckOnBehalf::Ack { because } =
                acks.judge(work, &announced.ask, self.now, &facts).await?
            {
                announced.ask.acked_by_dibs = Some(because);
            }
        }
        Ok(sent)
    }

    async fn send(
        &self,
        work: &mut Work,
        names: &Names,
        due: AskDue,
        reason: &str,
    ) -> Result<Announced, AppError> {
        let project = work.project_id();
        let item = ItemName::of(&existing_item(work, due.item).await?, self.schemes);
        let from = names.handle(due.from)?;
        let to = names.handle(due.to)?;
        let posted = Posted::next(self.ids);
        let posted = SystemPost {
            ids: posted,
            subject: subject(&from, &due.areas, &item),
            item: Some(due.item),
            on_behalf_of: Author::Member(due.from),
            to: Recipients::Members([due.to].into()),
            body: body(&from, &due.areas, &item, reason, posted.message)?,
            importance: Importance::High,
            ack_required: true,
        }
        .send(work, self.now)
        .await?;
        let ask = WideAsk {
            message: posted.message,
            item: due.item,
            from: due.from,
            to: due.to,
            areas: due.areas,
            acked_by_dibs: None,
        };
        work.insert_wide_ask(project, &ask).await?;
        Ok(Announced {
            ask,
            thread: posted.thread,
            to,
        })
    }
}

/// What dibs reads, inside one transaction of one project, to judge announcements on their
/// holders' behalf.
pub(crate) struct AckFacts<'a> {
    /// The project's claims.
    pub(crate) claims: &'a [Claim],
    /// The revokes that stick.
    pub(crate) blocks: &'a [GrantBlock],
    /// The project's footprints, listed without their paths.
    pub(crate) listed: &'a [Footprint],
}

/// dibs acknowledging announcements on their holders' behalf, inside the caller's transaction, by
/// the test it grants requests by.
pub(crate) struct DibsAcks<'a> {
    /// Where new ids come from.
    pub(crate) ids: &'a dyn IdGen,
    /// How items are named.
    pub(crate) schemes: &'a RefSchemeRegistry,
    /// The project's settings.
    pub(crate) settings: &'a ProjectSettings,
    /// When.
    pub(crate) now: OffsetDateTime,
}

impl DibsAcks<'_> {
    /// Judges `ask`, sent at `sent_at` (see [`decide_ack_on_holders_behalf`]), acts on the outcome,
    /// and answers it. A revoke that sticks in any judged area is looked for first: when one
    /// stands, no footprint is read. Otherwise the footprints of the holder's claims that hold the
    /// judged areas are read, with their paths, only for a claim not in review, the one reason that
    /// needs none.
    ///
    /// Acknowledging it grants the wide claim's member what the holder's acknowledgement would,
    /// each grant recording dibs's reason, and marks it acknowledged by dibs, so that it is
    /// answered everywhere and never judged again. No acknowledgement of the holder's is written:
    /// the holder did not read it. dibs says so, why, and how to undo the grants in its
    /// conversation, which the wide claim's member started and the holder was sent.
    ///
    /// One a revoke leaves to the holder is marked so, and dibs says so once in its conversation:
    /// dibs never acknowledges it, and only settles it once it closes
    /// ([`DibsAcks::settle_if_closed`]) or the revoke stops applying. One that closed is settled and never
    /// judged again. The holder may still acknowledge either. One that waits or has lapsed is left
    /// as it is.
    pub(crate) async fn judge(
        &self,
        work: &mut Work,
        ask: &WideAsk,
        sent_at: OffsetDateTime,
        facts: &AckFacts<'_>,
    ) -> Result<AckOnBehalf, AppError> {
        let judged = judged_areas(ask, facts.claims, self.now, self.settings.stale_after());
        let revoked: BTreeSet<QualifiedArea> = judged
            .iter()
            .filter(|(area, _)| {
                facts
                    .blocks
                    .iter()
                    .any(|block| block.applies(ask.from, ask.to, area, facts.claims))
            })
            .map(|(area, _)| area.clone())
            .collect();
        let blocked = !revoked.is_empty();
        let mut stored: BTreeMap<ItemId, ClaimFootprints> = BTreeMap::new();
        for (_, holding) in judged.iter().filter(|_| !blocked) {
            if !holding.state.is_in_review() && !stored.contains_key(&holding.item) {
                let read = ClaimFootprints::read(work, holding.item, facts.listed).await?;
                stored.insert(holding.item, read);
            }
        }
        let footprints: BTreeMap<ItemId, Vec<FootprintPaths<'_>>> = stored
            .iter()
            .map(|(item, read)| (*item, read.paths()))
            .collect();
        let judged = WideAckFacts {
            claims: facts.claims,
            footprints: &footprints,
            blocks: facts.blocks,
        };
        let outcome = decide_ack_on_holders_behalf(ask, sent_at, &judged, self.settings, self.now);
        let project = work.project_id();
        match outcome {
            AckOnBehalf::Ack { because } => self.acknowledge(work, ask, because).await?,
            AckOnBehalf::LeftToHolder => {
                work.leave_wide_ask_to_holder(project, ask.message).await?;
                self.say_left(work, ask, &revoked).await?;
            }
            AckOnBehalf::Closed => {
                work.settle_wide_ask(project, ask.message, self.now).await?;
            }
            AckOnBehalf::Wait { .. } | AckOnBehalf::Lapsed => {}
        }
        Ok(outcome)
    }

    /// Settles `ask`, sent at `sent_at`, when it has closed (see [`decide_ack_on_holders_behalf`]):
    /// its wide claim ended, or its holder holds none of its areas through any claim. Closing is
    /// decided before anything else, a revoke that sticks included, so no footprint is read for it,
    /// and nothing is acknowledged. It is all dibs judges of an announcement left to its holder.
    /// Answers whether it closed.
    pub(crate) async fn settle_if_closed(
        &self,
        work: &mut Work,
        ask: &WideAsk,
        sent_at: OffsetDateTime,
        facts: &AckFacts<'_>,
    ) -> Result<bool, AppError> {
        let judged = WideAckFacts {
            claims: facts.claims,
            footprints: &BTreeMap::new(),
            blocks: facts.blocks,
        };
        let closed = decide_ack_on_holders_behalf(ask, sent_at, &judged, self.settings, self.now)
            == AckOnBehalf::Closed;
        if closed {
            let project = work.project_id();
            work.settle_wide_ask(project, ask.message, self.now).await?;
        }
        Ok(closed)
    }

    /// Acknowledges `ask` on its holder's behalf because of `because`.
    async fn acknowledge(
        &self,
        work: &mut Work,
        ask: &WideAsk,
        because: GrantDecision,
    ) -> Result<(), AppError> {
        let project = work.project_id();
        let granted =
            store_grants_by_ack(work, self.ids, ask, because, self.settings, self.now).await?;
        work.mark_wide_ask_acked_by_dibs(project, ask.message, self.now, because)
            .await?;
        let Some(why) = because.why() else {
            return Ok(());
        };
        let thread = announcement_thread(work, ask).await?;
        let names = Names::load(work).await?;
        let item = ItemName::of(&existing_item(work, ask.item).await?, self.schemes);
        let text = AckedText {
            from: &names.handle(ask.from)?,
            holder: &names.handle(ask.to)?,
            item: &item,
            why,
            granted: &granted,
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
        Ok(())
    }

    /// Says in `ask`'s conversation, which both members read, that dibs leaves it to the holder
    /// because a revoke of theirs to the wide claim's member stands in `revoked`.
    async fn say_left(
        &self,
        work: &mut Work,
        ask: &WideAsk,
        revoked: &BTreeSet<QualifiedArea>,
    ) -> Result<(), AppError> {
        let thread = announcement_thread(work, ask).await?;
        let names = Names::load(work).await?;
        let text = LeftText {
            from: &names.handle(ask.from)?,
            holder: &names.handle(ask.to)?,
            revoked,
        };
        SystemReply {
            thread,
            message: MessageId(self.ids.next()),
            body: titled_body(&text.lead(), None)?,
            importance: Importance::Normal,
            ack_required: false,
        }
        .send(work, self.now)
        .await
    }
}

/// The conversation that carries `ask`.
async fn announcement_thread(work: &mut Work, ask: &WideAsk) -> Result<ThreadId, AppError> {
    let project = work.project_id();
    Ok(work
        .message(project, ask.message)
        .await?
        .ok_or_else(|| StoreError::Corrupt {
            what: "the message that carries an announcement is not stored".to_owned(),
        })?
        .thread)
}

/// The grants a holder's acknowledgement of a wide claim's announcement makes, stored with one
/// event each, inside the caller's transaction. A message that carries no announcement grants
/// nothing.
pub(crate) async fn grant_by_ack(
    work: &mut Work,
    ids: &dyn IdGen,
    message: MessageId,
    settings: &ProjectSettings,
    now: OffsetDateTime,
) -> Result<Vec<NamedGrant>, AppError> {
    let project = work.project_id();
    let Some(ask) = work.wide_ask(project, message).await? else {
        return Ok(Vec::new());
    };
    store_grants_by_ack(work, ids, &ask, GrantDecision::Holder, settings, now).await
}

/// The grants an acknowledgement of `ask` makes (see [`grants_by_ack`]), each stored as decided by
/// `decided` with a `wide_granted_by_ack` event that names it, inside the caller's transaction.
async fn store_grants_by_ack(
    work: &mut Work,
    ids: &dyn IdGen,
    ask: &WideAsk,
    decided: GrantDecision,
    settings: &ProjectSettings,
    now: OffsetDateTime,
) -> Result<Vec<NamedGrant>, AppError> {
    let project = work.project_id();
    let claims = work.claims(project).await?;
    let existing: Vec<Grant> = work
        .grants(project)
        .await?
        .into_iter()
        .map(|record| record.grant)
        .collect();
    let granted = grants_by_ack(ask, &existing, &claims, now, settings);
    if granted.is_empty() {
        return Ok(Vec::new());
    }
    let names = Names::load(work).await?;
    let mut named = Vec::with_capacity(granted.len());
    for grant in granted {
        let record = GrantRecord {
            id: GrantId(ids.next()),
            grant,
            created_at: now,
            decided,
        };
        work.insert_grant(project, &record).await?;
        work.record(
            now,
            EventKind::WideGrantedByAck(WideGrantedByAck {
                grant: record.id,
                item: record.grant.item,
                area: record.grant.area.clone(),
                by: record.grant.from,
                to: record.grant.to,
                decided,
            }),
        )
        .await?;
        named.push(names.grant(record)?);
    }
    Ok(named)
}

/// What dibs writes in an announcement's conversation when it acknowledges the announcement on
/// the holder's behalf.
struct AckedText<'a> {
    /// The wide claim's member, who may now edit.
    from: &'a Handle,
    /// The holder whose acknowledgement and grants they are.
    holder: &'a Handle,
    /// The wide claim's item.
    item: &'a ItemName,
    /// Why dibs acknowledged it, as [`GrantDecision::why`] says it.
    why: &'a str,
    /// The grants the acknowledgement made.
    granted: &'a [NamedGrant],
}

impl AckedText<'_> {
    /// That dibs acknowledged it on the holder's behalf and why; the grants it made, by id and
    /// area; that the order rule still decides which change lands first where both meet; and who
    /// may undo a grant, and how.
    fn lead(&self) -> String {
        let mut text = format!(
            "Acknowledged by dibs on {holder}'s behalf, because {why}.",
            holder = self.holder,
            why = self.why,
        );
        let Some(first) = self.granted.first() else {
            let _ = write!(
                text,
                " It made no new grant: {from} may already edit there for work item {label} \"{title}\".",
                from = self.from,
                label = self.item.label(),
                title = self.item.title,
            );
            return text;
        };
        let grants: Vec<String> = self
            .granted
            .iter()
            .map(|grant| format!("{} in {}", grant.record.id, grant.record.grant.area))
            .collect();
        let _ = write!(
            text,
            " {from} may edit there for work item {label} \"{title}\": grant {grants}. Where both changes meet in a file, the order rule decides which lands first. To undo a grant, {holder} or an admin revokes it (dibs revoke-grant {id}, or DELETE /v1/grants/{id}).",
            from = self.from,
            label = self.item.label(),
            title = self.item.title,
            grants = grants.join(", grant "),
            holder = self.holder,
            id = first.record.id,
        );
        text
    }
}

/// What dibs writes in an announcement's conversation when a revoke leaves it to the holder.
struct LeftText<'a> {
    /// The wide claim's member.
    from: &'a Handle,
    /// The holder whose revoke stands.
    holder: &'a Handle,
    /// The areas of the announcement where it stands.
    revoked: &'a BTreeSet<QualifiedArea>,
}

impl LeftText<'_> {
    /// That a revoke of the holder's stands for the wide claim's member in those areas, so dibs
    /// leaves the announcement to the holder to acknowledge or answer.
    fn lead(&self) -> String {
        format!(
            "Left to {holder} by dibs: a revoke of {holder}'s grant to {from} in {areas} stands while both claims last, so dibs leaves this announcement to {holder} to acknowledge or answer.",
            holder = self.holder,
            from = self.from,
            areas = area_list(self.revoked),
        )
    }
}

/// The subject: who asks, for which areas, about which item.
fn subject(from: &Handle, areas: &BTreeSet<QualifiedArea>, item: &ItemName) -> String {
    format!(
        "Wide change by {from} on {}: may they edit {}?",
        item.label(),
        area_list(areas)
    )
}

/// The message: that dibs sends it for the wide claim's member, why, which of the holder's areas
/// it is about, what acknowledging it does, and when dibs acknowledges it on the holder's behalf.
fn body(
    from: &Handle,
    areas: &BTreeSet<QualifiedArea>,
    item: &ItemName,
    reason: &str,
    message: MessageId,
) -> Result<Body, AppError> {
    let listed = area_list(areas);
    let mut text = format!(
        "dibs sent this for {from}, who took a wide claim on work item {} \"{}\" for a change across several areas.",
        item.label(),
        item.title
    );
    if !reason.is_empty() {
        let _ = write!(text, "\nWhy: {reason}");
    }
    let _ = write!(
        text,
        "\nYou hold {listed}. A wide claim holds none of the areas it names, so {from}'s edits there wait for your answer.\
         \nAcknowledge this message (dibs_ack, or dibs ack {message}) to grant {from} edits in {listed} for that work item; the grant ends when either claim ends.\
         \ndibs acknowledges it on your behalf at once when your claim's branch changes nothing in those areas, the areas lie idle on your claim, or your claim is in review, and otherwise once the project's grant_after has passed. Until then you may acknowledge it, or reply in this thread with what your work still changes there.\
         \n{TITLE_NOTE}"
    );
    Body::parse(&text).map_err(|_| unwritable("the text of an announcement"))
}

/// The areas by name, the first [`LISTED_AREAS`] of them, with a count of the rest.
fn area_list(areas: &BTreeSet<QualifiedArea>) -> String {
    let named: Vec<String> = areas
        .iter()
        .take(LISTED_AREAS)
        .map(ToString::to_string)
        .collect();
    let mut text = named.join(", ");
    if areas.len() > LISTED_AREAS {
        let _ = write!(text, " and {} more", areas.len() - LISTED_AREAS);
    }
    text
}

/// A text this module composes broke the rules of its kind: something stored holds a value it
/// cannot carry.
fn unwritable(what: &str) -> AppError {
    AppError::Store(StoreError::Corrupt {
        what: format!("{what} could not be written from the stored records"),
    })
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use dibs_domain::claim::ClaimRefused;
    use dibs_domain::edit::{HeldReason, Verdict};
    use dibs_domain::event::{GrantEnd, GrantRevoked, WideClaimed};
    use dibs_domain::ids::ItemId;
    use dibs_domain::member::Role;
    use dibs_domain::paths::RepoPath;

    use super::*;
    use crate::actor::Actor;
    use crate::named::Refusal;
    use crate::records::MessageRecord;
    use crate::use_cases::testing::{Env, Person, new_member, session};
    use crate::use_cases::{
        AckMessage, Authenticate, ClaimItem, ClaimItemInput, ClaimMade, CreateMember,
        CreateMemberInput, EditCheck, EditCheckInput, Release, Status, TakeOver, TakeOverInput,
        Unacked, WideInput,
    };

    fn area(text: &str) -> QualifiedArea {
        QualifiedArea::parse(text).unwrap()
    }

    async fn carol(env: &Env) -> Person {
        let created = CreateMember::new(&env.deps)
            .execute(
                &env.alice.actor,
                CreateMemberInput {
                    member: new_member("carol"),
                    role: Role::Member,
                },
            )
            .await
            .unwrap();
        let actor = Authenticate::new(&env.deps)
            .execute(created.token.expose())
            .await
            .unwrap();
        let session = session(&env.deps, &actor, "tower", "tower/0c0ffee0").await;
        Person { actor, session }
    }

    async fn claim(env: &Env, who: &Person, item: ItemId, areas: &[&str]) -> ClaimMade {
        ClaimItem::new(&env.deps)
            .execute(
                &who.actor,
                ClaimItemInput {
                    branches: Default::default(),
                    item,
                    session: who.session,
                    areas: areas.iter().map(|a| (*a).to_owned()).collect(),
                    wide: None,
                    kind: None,
                },
            )
            .await
            .unwrap()
    }

    fn wide_input(item: ItemId, who: &Person, named: &[&str]) -> ClaimItemInput {
        ClaimItemInput {
            branches: Default::default(),
            item,
            session: who.session,
            areas: Vec::new(),
            wide: Some(WideInput {
                areas: named.iter().map(|a| (*a).to_owned()).collect(),
                reason: dibs_domain::wide::WideReason::parse("Upgrade the HTTP client").unwrap(),
            }),
            kind: None,
        }
    }

    async fn wide(env: &Env, who: &Person, item: ItemId, named: &[&str]) -> ClaimMade {
        ClaimItem::new(&env.deps)
            .execute(&who.actor, wide_input(item, who, named))
            .await
            .unwrap()
    }

    async fn unacked(env: &Env, who: &Actor) -> Vec<MessageRecord> {
        unacked_with_threads(env, who)
            .await
            .into_iter()
            .map(|(message, _)| message)
            .collect()
    }

    async fn unacked_with_threads(env: &Env, who: &Actor) -> Vec<(MessageRecord, Author)> {
        Unacked::new(&env.deps)
            .execute(who)
            .await
            .unwrap()
            .into_iter()
            .map(|unacked| (unacked.message.record, unacked.thread.record.author))
            .collect()
    }

    async fn verdict(env: &Env, who: &Person, path: &str) -> Verdict {
        EditCheck::new(&env.deps)
            .execute(
                &who.actor,
                EditCheckInput {
                    session: who.session,
                    paths: vec![RepoPath::parse(path).unwrap()],
                    reserve: true,
                    checkout: None,
                    worktree: None,
                },
            )
            .await
            .unwrap()
            .verdicts
            .remove(0)
            .verdict
            .verdict
    }

    #[tokio::test]
    async fn a_wide_claim_announces_once_to_each_holder_and_never_to_its_member() {
        let env = Env::new().await;
        let carol = carol(&env).await;
        let alices = env.item("gh:acme/widgets#1").await;
        let carols = env.item("gh:acme/widgets#2").await;
        let bobs = env.item("gh:acme/widgets#3").await;
        claim(&env, &env.alice, alices, &["billing"]).await;
        claim(&env, &carol, carols, &["search"]).await;
        let before = env.events().await.len();
        let made = wide(&env, &env.bob, bobs, &["billing", "search"]).await;
        let claim = made.outcome.claim();
        assert!(
            claim.areas.is_empty(),
            "a wide claim holds none of its areas"
        );
        let asked: Vec<(&str, BTreeSet<QualifiedArea>)> = made
            .announced
            .iter()
            .map(|a| (a.to.as_str(), a.ask.areas.clone()))
            .collect();
        assert_eq!(
            asked,
            [
                ("alice", [area("default:billing")].into()),
                ("carol", [area("default:search")].into()),
            ]
        );
        for who in [&env.alice.actor, &carol.actor] {
            let found = unacked_with_threads(&env, who).await;
            assert_eq!(found.len(), 1, "one announcement each");
            let (message, thread_author) = &found[0];
            assert_eq!(*thread_author, Author::Member(env.bob.actor.member));
            let messages = [message.clone()];
            assert_eq!(messages[0].importance, Importance::High);
            assert!(messages[0].ack_required);
            assert_eq!(
                messages[0].author,
                Author::System,
                "dibs writes it, for bob"
            );
            assert_eq!(messages[0].label.as_str(), "dibs");
            assert!(messages[0].body.as_str().contains("dibs sent this for bob"));
        }
        assert!(unacked(&env, &env.bob.actor).await.is_empty());
        let kinds = env.kinds_after(before).await;
        assert!(kinds.contains(&EventKind::WideClaimed(WideClaimed {
            item: bobs,
            member: env.bob.actor.member,
            areas: [area("default:billing"), area("default:search")].into(),
        })));
        let again = wide(&env, &env.bob, bobs, &["billing"]).await;
        assert!(again.announced.is_empty(), "a renewal asks nobody again");
        assert_eq!(unacked(&env, &env.alice.actor).await.len(), 1);
    }

    #[tokio::test]
    async fn the_wide_claimers_edit_waits_for_the_holders_ack_which_grants_it() {
        let env = Env::new().await;
        let alices = env.item("gh:acme/widgets#1").await;
        let bobs = env.item("gh:acme/widgets#2").await;
        claim(&env, &env.alice, alices, &["billing"]).await;
        let made = wide(&env, &env.bob, bobs, &["billing", "search"]).await;
        let path = "default:packages/billing/src/Invoice.php";
        assert!(matches!(
            verdict(&env, &env.bob, path).await,
            Verdict::HeldBy {
                reason: HeldReason::AwaitingAck,
                ..
            }
        ));
        assert_eq!(
            verdict(&env, &env.bob, "default:packages/search/src/Index.php").await,
            Verdict::Allowed,
            "an area nobody holds is free to the wide claimer"
        );
        let message = made.announced[0].ask.message;
        let before = env.events().await.len();
        let acked = AckMessage::new(&env.deps)
            .execute(&env.alice.actor, message)
            .await
            .unwrap();
        assert_eq!(acked.granted.len(), 1);
        let grant = &acked.granted[0];
        assert_eq!(
            (
                grant.record.grant.item,
                grant.from.as_str(),
                grant.to.as_str()
            ),
            (bobs, "alice", "bob")
        );
        let kinds = env.kinds_after(before).await;
        assert_eq!(
            kinds.last(),
            Some(&EventKind::WideGrantedByAck(WideGrantedByAck {
                grant: grant.record.id,
                item: bobs,
                area: area("default:billing"),
                by: env.alice.actor.member,
                to: env.bob.actor.member,
                decided: GrantDecision::Holder,
            }))
        );
        assert_eq!(
            verdict(&env, &env.bob, path).await,
            Verdict::Granted {
                by: env.alice.actor.member,
                area: area("default:billing"),
            }
        );
        let repeat = AckMessage::new(&env.deps)
            .execute(&env.alice.actor, message)
            .await
            .unwrap();
        assert!(repeat.granted.is_empty(), "a repeat grants nothing again");
        let status = Status::new(&env.deps)
            .execute(&env.alice.actor)
            .await
            .unwrap();
        assert_eq!(status.wide.len(), 1);
        let rows: Vec<(String, Option<&str>, bool, Option<GrantDecision>)> = status.wide[0]
            .areas
            .iter()
            .map(|row| {
                (
                    row.area.to_string(),
                    row.holder.as_ref().map(|(h, _)| h.as_str()),
                    row.granted,
                    row.decided,
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                (
                    "default:billing".to_owned(),
                    Some("alice"),
                    true,
                    Some(GrantDecision::Holder)
                ),
                ("default:search".to_owned(), None, false, None),
            ],
            "alice's own acknowledgement is the holder's decision"
        );
        assert!(
            status.holds.iter().all(|hold| hold.item != bobs),
            "the wide claim holds nothing"
        );
    }

    #[tokio::test]
    async fn the_grant_by_ack_ends_with_the_holders_claim() {
        let env = Env::new().await;
        let alices = env.item("gh:acme/widgets#1").await;
        let bobs = env.item("gh:acme/widgets#2").await;
        claim(&env, &env.alice, alices, &["billing"]).await;
        let made = wide(&env, &env.bob, bobs, &["billing"]).await;
        let acked = AckMessage::new(&env.deps)
            .execute(&env.alice.actor, made.announced[0].ask.message)
            .await
            .unwrap();
        let before = env.events().await.len();
        Release::new(&env.deps)
            .execute(&env.alice.actor, alices)
            .await
            .unwrap();
        assert!(
            env.kinds_after(before)
                .await
                .contains(&EventKind::GrantRevoked(GrantRevoked {
                    grant: acked.granted[0].record.id,
                    item: bobs,
                    area: area("default:billing"),
                    from: env.alice.actor.member,
                    to: env.bob.actor.member,
                    cause: GrantEnd::GrantorLeft,
                    by: None,
                }))
        );
    }

    #[tokio::test]
    async fn a_member_who_claims_inside_a_live_wide_claim_is_told_and_asked_at_once() {
        let env = Env::new().await;
        let bobs = env.item("gh:acme/widgets#1").await;
        let alices = env.item("gh:acme/widgets#2").await;
        wide(&env, &env.bob, bobs, &["billing"]).await;
        let before = env.events().await.len();
        let made = claim(&env, &env.alice, alices, &["billing"]).await;
        assert!(made.outcome.claim().holds(&area("default:billing")));
        let asked = unacked(&env, &env.alice.actor).await;
        assert_eq!(asked.len(), 1, "the new holder is sent the announcement");
        assert_eq!(made.inside_wide.len(), 1);
        let note = &made.inside_wide[0];
        assert_eq!(note.area, area("default:billing"));
        assert_eq!(note.holder.as_str(), "bob");
        assert_eq!(note.item.id, bobs);
        assert_eq!(note.message, Some(asked[0].id));
        assert!(
            made.announced.is_empty(),
            "nothing was announced in alice's name"
        );
        let kinds = env.kinds_after(before).await;
        assert!(
            kinds
                .iter()
                .any(|kind| matches!(kind, EventKind::MessagePosted(posted) if posted.message == asked[0].id)),
            "in the same transaction as the claim"
        );
        let renewed = claim(&env, &env.alice, alices, &["billing"]).await;
        assert_eq!(renewed.inside_wide[0].message, Some(asked[0].id));
        assert_eq!(unacked(&env, &env.alice.actor).await.len(), 1, "asked once");
    }

    #[tokio::test]
    async fn a_holder_who_comes_to_hold_an_area_by_editing_is_asked() {
        let env = Env::new().await;
        let bobs = env.item("gh:acme/widgets#1").await;
        let alices = env.item("gh:acme/widgets#2").await;
        wide(&env, &env.bob, bobs, &["billing"]).await;
        claim(&env, &env.alice, alices, &[]).await;
        assert!(unacked(&env, &env.alice.actor).await.is_empty());
        assert_eq!(
            verdict(&env, &env.alice, "default:packages/billing/a.php").await,
            Verdict::Allowed
        );
        assert_eq!(unacked(&env, &env.alice.actor).await.len(), 1);
    }

    #[tokio::test]
    async fn two_wide_claims_over_one_area_refuse_the_second_naming_the_first() {
        let env = Env::new().await;
        let bobs = env.item("gh:acme/widgets#1").await;
        let alices = env.item("gh:acme/widgets#2").await;
        wide(&env, &env.bob, bobs, &["billing"]).await;
        let error = ClaimItem::new(&env.deps)
            .execute(
                &env.alice.actor,
                wide_input(alices, &env.alice, &["search", "billing"]),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), "wide_overlap");
        assert!(matches!(
            &error,
            AppError::Claim(Refusal {
                reason: ClaimRefused::WideOverlap { item, .. },
                member: Some(handle),
                ..
            }) if *item == bobs && handle.as_str() == "bob"
        ));
        let empty = ClaimItem::new(&env.deps)
            .execute(&env.alice.actor, wide_input(alices, &env.alice, &[]))
            .await
            .unwrap_err();
        assert_eq!(empty.code(), "invalid_wide");
    }

    #[tokio::test]
    async fn a_takeover_carries_the_wide_scope_and_asks_again_in_the_new_name() {
        let env = Env::new().await;
        let carol = carol(&env).await;
        let bobs = env.item("gh:acme/widgets#1").await;
        let carols = env.item("gh:acme/widgets#2").await;
        wide(&env, &env.bob, bobs, &["billing"]).await;
        env.advance(Duration::hours(25));
        claim(&env, &carol, carols, &["billing"]).await;
        assert!(
            unacked(&env, &carol.actor).await.is_empty(),
            "a stale wide claim asks nobody"
        );
        let taken = TakeOver::new(&env.deps)
            .execute(
                &env.alice.actor,
                TakeOverInput {
                    item: bobs,
                    session: env.alice.session,
                    areas: Vec::new(),
                    reason: Some(Body::parse("Bob is away; I finish the upgrade.").unwrap()),
                    kind: None,
                },
            )
            .await
            .unwrap();
        assert!(taken.takeover.claim.names_wide(&area("default:billing")));
        assert_eq!(taken.announced.len(), 1);
        let asked = unacked_with_threads(&env, &carol.actor).await;
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].1, Author::Member(env.alice.actor.member));
        assert_eq!(asked[0].0.author, Author::System);
    }
}
