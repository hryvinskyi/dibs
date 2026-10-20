//! Escalating what agents do not agree on to the project's arbiters, and the escalations that still
//! wait for one of them.

use std::collections::BTreeSet;
use std::sync::Arc;

use time::OffsetDateTime;

use dibs_domain::escalation::{
    EscalationReason, EscalationSubject, Parties, Unanswered, escalation_reach, item_parties,
    slot_parties, thread_parties,
};
use dibs_domain::event::{Escalated, EventKind};
use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{MemberId, MessageId, ThreadId};
use dibs_domain::message::{AckState, Author, Importance, Recipients, speaks_for};
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_domain::slot::SlotRefused;
use dibs_domain::wide::ask_stands;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{ItemName, Names, refuse};
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::records::{EscalationRecord, ItemRecord, MemberRecord, ThreadRecord};
use crate::use_cases::acks::{people, reached};
use crate::use_cases::common::existing_item;
use crate::use_cases::readable::{may_read_thread, readable_thread};
use crate::use_cases::slots::Scene;
use crate::use_cases::writing::{Posted, SystemPost, system_body};
use crate::work::Work;

/// What to escalate, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscalateInput {
    /// What it is about.
    pub subject: EscalationSubject,
    /// Why, as the member writes it to the arbiters.
    pub reason: EscalationReason,
}

/// An escalation sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Escalation {
    /// The conversation it started.
    pub thread: ThreadId,
    /// Its message, which an arbiter acknowledges to settle it.
    pub message: MessageId,
    /// The arbiters it reached, ordered by handle.
    pub arbiters: Vec<Handle>,
    /// Everyone it reached, the arbiters included, ordered by handle.
    pub reached: Vec<Handle>,
}

/// An escalation no arbiter has acknowledged yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenEscalation {
    /// What it is about.
    pub subject: EscalationSubject,
    /// How the subject reads to a person: an item's reference and title, a slot's name, a
    /// conversation's subject.
    pub about: String,
    /// The member who escalated.
    pub by: Handle,
    /// The arbiters it waits for, ordered by handle.
    pub arbiters: Vec<Handle>,
    /// Its conversation.
    pub thread: ThreadId,
    /// Its message.
    pub message: MessageId,
    /// When.
    pub at: OffsetDateTime,
}

/// Sends an escalation: an `urgent` message dibs writes for the member escalating, asking each of
/// its recipients to acknowledge it, to the project's arbiters and to the subject's holder and
/// requester — the member whose action caused it: the first member waiting for a slot, the author
/// of a conversation's message that went unacknowledged, or for a work item the member escalating.
/// It is never counted toward anyone's limit on raised messages. The escalation stands, shown on
/// the board to those it reached, until one of the arbiters acknowledges it.
pub struct Escalate {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
    ref_schemes: Arc<RefSchemeRegistry>,
}

impl Escalate {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            ids: deps.ids.clone(),
            notifier: deps.notifier.clone(),
            hot_rules: deps.hot_rules.clone(),
            ref_schemes: deps.ref_schemes.clone(),
        }
    }

    /// Sends the escalation.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] for an unknown item or a
    /// conversation the caller may not read; [`AppError::Slot`] for an undeclared slot;
    /// [`AppError::Escalation`] when the project names no arbiter but the caller;
    /// [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        input: EscalateInput,
    ) -> Result<Escalation, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let settings = work.project_settings(&self.hot_rules).await?;
        let (parties, about) = self.parties(&mut work, actor, &input.subject, now).await?;
        let sent = Case {
            subject: input.subject,
            about,
            parties,
            by: actor.member,
            reason: &input.reason,
        }
        .send(&mut work, &*self.ids, settings.arbiters(), now)
        .await?;
        work.commit(&*self.notifier).await?;
        Ok(sent)
    }

    /// Who the subject concerns, and how it reads to a person.
    async fn parties(
        &self,
        work: &mut Work,
        actor: &Actor,
        subject: &EscalationSubject,
        now: OffsetDateTime,
    ) -> Result<(Parties, String), AppError> {
        let project = work.project_id();
        match subject {
            EscalationSubject::Item(id) => {
                let item = existing_item(work, *id).await?;
                let holder = work
                    .claim(project, item.id)
                    .await?
                    .map(|claim| claim.member);
                Ok((item_parties(holder), item_about(&item, &self.ref_schemes)))
            }
            EscalationSubject::Slot(slot) => {
                let mut scene = Scene::load(work, &self.hot_rules, now).await?;
                let Some(standing) = scene.board.standing(scene.catalog(), slot) else {
                    return Err(
                        refuse(work, SlotRefused::UnknownSlot { slot: slot.clone() }).await,
                    );
                };
                scene.save(work, &*self.ids, now).await?;
                Ok((slot_parties(&standing), format!("the slot {slot}")))
            }
            EscalationSubject::Thread(id) => {
                let thread = readable_thread(work, actor, *id).await?;
                let holder = match thread.item {
                    Some(item) => work.claim(project, item).await?.map(|claim| claim.member),
                    None => None,
                };
                let unanswered = newest_unanswered(work, &thread).await?;
                Ok((
                    thread_parties(holder, unanswered.as_ref()),
                    conversation_about(thread.id),
                ))
            }
        }
    }
}

/// One escalation to send inside the caller's transaction: what it is about and who it concerns,
/// already found, the member it is sent for, and their reason. The one way an escalation is
/// written, whether a member asks for it or the service raises it for them.
pub(crate) struct Case<'a> {
    pub(crate) subject: EscalationSubject,
    /// How the subject reads to a person.
    pub(crate) about: String,
    pub(crate) parties: Parties,
    /// The member the escalation is sent for.
    pub(crate) by: MemberId,
    pub(crate) reason: &'a EscalationReason,
}

impl Case<'_> {
    /// Writes the escalation: an `urgent` message from dibs, asking to be acknowledged, to the
    /// arbiters named by `arbiters` who are still people of the project and to the subject's
    /// parties; the record that it stands until one of those arbiters acknowledges it; and
    /// `escalated`.
    ///
    /// # Errors
    /// [`AppError::AlreadyEscalated`] while an escalation of the same subject waits for an
    /// arbiter; [`AppError::Escalation`] when no arbiter but `by` is a person of the project;
    /// [`AppError::Store`] when the store fails.
    pub(crate) async fn send(
        self,
        work: &mut Work,
        ids: &dyn IdGen,
        arbiters: &BTreeSet<Handle>,
        now: OffsetDateTime,
    ) -> Result<Escalation, AppError> {
        let project = work.project_id();
        if let Some(standing) = work
            .open_escalations(project)
            .await?
            .into_iter()
            .find(|open| open.subject == self.subject)
        {
            return Err(AppError::AlreadyEscalated {
                thread: standing.thread,
            });
        }
        let members = work.members(project).await?;
        let present: BTreeSet<MemberId> = members
            .iter()
            .filter(|member| takes_part(member))
            .map(|member| member.id)
            .collect();
        let named: BTreeSet<MemberId> = members
            .iter()
            .filter(|member| arbiters.contains(&member.handle))
            .map(|member| member.id)
            .collect();
        let reach = escalation_reach(&named, &self.parties, self.by, &present)?;
        let judges: BTreeSet<MemberId> = named.intersection(&reach).copied().collect();
        let names = Names::load(work).await?;
        let by = names.handle(self.by)?;
        let listed = |set: &BTreeSet<MemberId>| -> Result<Vec<Handle>, AppError> {
            let mut handles = set
                .iter()
                .map(|member| names.handle(*member))
                .collect::<Result<Vec<_>, _>>()?;
            handles.sort();
            Ok(handles)
        };
        let arbiter_handles = listed(&judges)?;
        let holder_handles = listed(&self.parties.holders)?;
        let requester = names.handle(self.parties.requester.unwrap_or(self.by))?;
        let posted = Posted::next(ids);
        let lead = lead(
            &by,
            &self.about,
            &arbiter_handles,
            &holder_handles,
            &requester,
            posted.message,
        );
        SystemPost {
            ids: posted,
            subject: format!("Escalated by {by}: {}", self.about),
            item: self.subject.item(),
            on_behalf_of: Author::Member(self.by),
            to: Recipients::members(reach.clone())?,
            body: system_body(&lead, Some(("Why", self.reason.as_str())))?,
            importance: Importance::Urgent,
            ack_required: true,
        }
        .send(work, now)
        .await?;
        work.insert_escalation(
            project,
            &EscalationRecord {
                message: posted.message,
                thread: posted.thread,
                subject: self.subject.clone(),
                by: self.by,
                arbiters: judges,
                created_at: now,
            },
        )
        .await?;
        work.record(
            now,
            EventKind::Escalated(Escalated {
                subject: self.subject,
                by: self.by,
            }),
        )
        .await?;
        Ok(Escalation {
            thread: posted.thread,
            message: posted.message,
            arbiters: arbiter_handles,
            reached: listed(&reach)?,
        })
    }
}

/// The newest message of a thread that asks for an acknowledgement some of its recipients have not
/// given — leaving out a wide claim's announcement whose claim has ended — with who asked and who
/// owes it.
async fn newest_unanswered(
    work: &mut Work,
    thread: &ThreadRecord,
) -> Result<Option<Unanswered>, AppError> {
    let project = work.project_id();
    let people = people(work).await?;
    let acks = work.thread_acks(project, thread.id).await?;
    let mut claims = None;
    let mut messages = work.messages(project, thread.id).await?;
    messages.reverse();
    for message in messages.iter().filter(|message| message.ack_required) {
        if let Some(ask) = work.wide_ask(project, message.id).await? {
            if claims.is_none() {
                claims = Some(work.claims(project).await?);
            }
            if !claims
                .as_deref()
                .is_some_and(|claims| ask_stands(&ask, claims))
            {
                continue;
            }
        }
        let of_message: Vec<_> = acks
            .iter()
            .filter(|ack| ack.message == message.id)
            .copied()
            .collect();
        let state = AckState::derive(&reached(thread, message, &people), &of_message);
        let awaiting: BTreeSet<MemberId> = state
            .entries()
            .filter(|(_, at)| at.is_none())
            .map(|(member, _)| member)
            .collect();
        if !awaiting.is_empty() {
            return Ok(Some(Unanswered {
                sender: speaks_for(thread.author, message.author),
                awaiting,
            }));
        }
    }
    Ok(None)
}

/// How a conversation reads in an escalation: by its id alone. An escalation reaches arbiters and
/// holders who may not read that conversation, so its subject never travels with it.
fn conversation_about(thread: ThreadId) -> String {
    format!("conversation #{thread}")
}

/// How an item reads in an escalation: its reference, or `#<id>`, and its title.
fn item_about(item: &ItemRecord, schemes: &RefSchemeRegistry) -> String {
    let name = ItemName::of(item, schemes);
    let label = name
        .external_ref
        .as_ref()
        .map_or_else(|| format!("#{}", name.id), ToString::to_string);
    format!("work item {label} \"{}\"", name.title)
}

/// What dibs says above the member's reason.
fn lead(
    by: &Handle,
    about: &str,
    arbiters: &[Handle],
    holders: &[Handle],
    requester: &Handle,
    message: MessageId,
) -> String {
    let join = |handles: &[Handle]| {
        handles
            .iter()
            .map(Handle::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let holders = if holders.is_empty() {
        "nobody".to_owned()
    } else {
        join(holders)
    };
    format!(
        "{by} asks the arbiters of this project ({}) to settle {about}: the agents involved do not agree.\
         \nHeld by: {holders}. Asked for by: {requester}.\
         \nAn arbiter answers in this conversation and acknowledges this message (dibs_ack, or dibs ack {message}); until one does, the board shows it.\
         \nMeanwhile no agent settles it alone: do not take over the claim or the slot, do not edit in the area, pick other work, and follow the arbiter's answer here.",
        join(arbiters)
    )
}

/// Lists the escalations the caller may read that no arbiter they reached has acknowledged yet,
/// newest first.
pub(crate) async fn open_escalations(
    work: &mut Work,
    actor: &Actor,
    names: &Names,
    schemes: &RefSchemeRegistry,
) -> Result<Vec<OpenEscalation>, AppError> {
    let project = work.project_id();
    let mut open = Vec::new();
    for escalation in work.open_escalations(project).await? {
        let Some(thread) = work.thread(project, escalation.thread).await? else {
            continue;
        };
        if !may_read_thread(actor, &thread) {
            continue;
        }
        let about = match &escalation.subject {
            EscalationSubject::Item(id) => match work.item(project, *id).await? {
                Some(item) => item_about(&item, schemes),
                None => format!("work item #{id}"),
            },
            EscalationSubject::Slot(slot) => format!("the slot {slot}"),
            EscalationSubject::Thread(id) => conversation_about(*id),
        };
        let mut arbiters = escalation
            .arbiters
            .iter()
            .map(|member| names.handle(*member))
            .collect::<Result<Vec<_>, _>>()?;
        arbiters.sort();
        open.push(OpenEscalation {
            subject: escalation.subject,
            about,
            by: names.handle(escalation.by)?,
            arbiters,
            thread: escalation.thread,
            message: escalation.message,
            at: escalation.created_at,
        });
    }
    Ok(open)
}

/// Whether a member takes part in an escalation: a person still in the project.
fn takes_part(member: &MemberRecord) -> bool {
    member.is_present() && member.role.is_person()
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use dibs_domain::member::Role;
    use dibs_domain::message::{Body, SessionLabel, Subject};
    use dibs_domain::slot::{Purpose, SlotName};

    use super::*;
    use crate::use_cases::testing::{Env, Person, new_member, session};
    use crate::use_cases::{
        AckMessage, Addressees, Authenticate, ClaimItem, ClaimItemInput, CreateMember,
        CreateMemberInput, HoldMode, ReadThread, StartThread, StartThreadInput, Status, TakeSlot,
        TakeSlotInput, Unacked,
    };

    const SETTINGS: &str = r#"{
        "raised_per_hour": 1,
        "arbiters": ["dave"],
        "areas": [{"name": "billing", "paths": ["packages/billing/**"]}],
        "slots": [{"name": "deploy:prod", "max_hold": "30m"}]
    }"#;

    async fn person(env: &Env, handle: &str) -> Person {
        let token = CreateMember::new(&env.deps)
            .execute(
                &env.alice.actor,
                CreateMemberInput {
                    member: new_member(handle),
                    role: Role::Member,
                },
            )
            .await
            .unwrap()
            .token;
        let actor = Authenticate::new(&env.deps)
            .execute(token.expose())
            .await
            .unwrap();
        let session = session(&env.deps, &actor, "box", &format!("box/{handle}")).await;
        Person { actor, session }
    }

    /// The project with an arbiter `dave` and a member `carol` beside alice and bob.
    async fn project() -> (Env, Person, Person) {
        let env = Env::with_settings(SETTINGS).await;
        let carol = person(&env, "carol").await;
        let dave = person(&env, "dave").await;
        (env, carol, dave)
    }

    fn reason(text: &str) -> EscalationReason {
        EscalationReason::parse(text).unwrap()
    }

    async fn escalate(
        env: &Env,
        by: &Actor,
        subject: EscalationSubject,
    ) -> Result<Escalation, AppError> {
        Escalate::new(&env.deps)
            .execute(
                by,
                EscalateInput {
                    subject,
                    reason: reason("We both need billing today; who goes first?"),
                },
            )
            .await
    }

    fn handles(list: &[Handle]) -> Vec<&str> {
        list.iter().map(Handle::as_str).collect()
    }

    #[tokio::test]
    async fn an_item_escalation_reaches_the_arbiters_and_its_holder_and_is_never_counted() {
        let (env, carol, dave) = project().await;
        let item = env.item("gh:acme/widgets#1").await;
        ClaimItem::new(&env.deps)
            .execute(
                &carol.actor,
                ClaimItemInput {
                    item,
                    session: carol.session,
                    areas: vec!["billing".to_owned()],
                    wide: None,
                },
            )
            .await
            .unwrap();
        let first = escalate(&env, &env.bob.actor, EscalationSubject::Item(item))
            .await
            .unwrap();
        assert_eq!(handles(&first.arbiters), ["dave"]);
        assert_eq!(
            handles(&first.reached),
            ["carol", "dave"],
            "bob asked, so he is the requester and writes it"
        );
        let again = escalate(&env, &env.bob.actor, EscalationSubject::Item(item))
            .await
            .unwrap_err();
        assert!(
            matches!(again, AppError::AlreadyEscalated { thread } if thread == first.thread),
            "{again:?}"
        );
        assert_eq!(again.code(), "already_escalated");
        let other = env.item("gh:acme/widgets#11").await;
        let second = escalate(&env, &env.bob.actor, EscalationSubject::Item(other)).await;
        assert!(
            second.is_ok(),
            "the limit of one raised message an hour never counts an escalation"
        );
        let mut tx = env.deps.store.begin().await.unwrap();
        assert!(
            tx.raised_since(
                env.project,
                env.bob.actor.member,
                env.now() - Duration::hours(1)
            )
            .await
            .unwrap()
            .is_empty()
        );
        drop(tx);

        let thread = ReadThread::new(&env.deps)
            .execute(&dave.actor, first.thread)
            .await
            .unwrap();
        let message = &thread.messages[0];
        assert!(message.author.is_dibs(), "dibs writes it, never a member");
        assert_eq!(message.record.label.as_str(), "dibs");
        assert_eq!(message.record.importance, Importance::Urgent);
        assert!(message.record.ack_required);
        assert_eq!(thread.thread.author.as_str(), "bob");
        assert_eq!(thread.thread.record.item, Some(item));
        let text = message.record.body.as_str();
        assert!(
            text.starts_with("bob asks the arbiters of this project (dave)"),
            "{text}"
        );
        assert!(
            text.contains("Held by: carol. Asked for by: bob."),
            "{text}"
        );
        assert!(text.ends_with("Why: We both need billing today; who goes first?"));
        let asked: Vec<String> = thread.acks[&first.message]
            .iter()
            .map(|ack| ack.handle.to_string())
            .collect();
        assert_eq!(asked, ["carol", "dave"]);

        let unacked = Unacked::new(&env.deps).execute(&carol.actor).await.unwrap();
        assert_eq!(
            unacked.len(),
            1,
            "the escalation of the item she holds asks carol; the other item is nobody's"
        );
        assert!(
            Unacked::new(&env.deps)
                .execute(&env.bob.actor)
                .await
                .unwrap()
                .is_empty(),
            "nobody asks bob to acknowledge his own escalation"
        );
        let kinds = env.kinds_after(0).await;
        assert!(kinds.contains(&EventKind::Escalated(Escalated {
            subject: EscalationSubject::Item(item),
            by: env.bob.actor.member,
        })));
    }

    #[tokio::test]
    async fn a_slot_escalation_reaches_its_holder_and_the_member_waiting_first() {
        let (env, carol, _dave) = project().await;
        let slot = SlotName::parse("deploy:prod").unwrap();
        for who in [&carol, &env.bob] {
            TakeSlot::new(&env.deps)
                .execute(
                    &who.actor,
                    TakeSlotInput {
                        slot: slot.clone(),
                        session: who.session,
                        purpose: Purpose::parse("ship").unwrap(),
                        item: None,
                        mode: HoldMode::Session,
                    },
                )
                .await
                .unwrap();
        }
        let sent = escalate(&env, &env.alice.actor, EscalationSubject::Slot(slot))
            .await
            .unwrap();
        assert_eq!(handles(&sent.reached), ["bob", "carol", "dave"]);
        let unknown = escalate(
            &env,
            &env.alice.actor,
            EscalationSubject::Slot(SlotName::parse("ghost").unwrap()),
        )
        .await
        .unwrap_err();
        assert_eq!(unknown.code(), "unknown_slot");
    }

    #[tokio::test]
    async fn a_conversation_escalation_reaches_the_sender_of_the_message_left_unacknowledged() {
        let (env, carol, dave) = project().await;
        let started = StartThread::new(&env.deps)
            .execute(
                &env.bob.actor,
                StartThreadInput {
                    subject: Subject::parse("May I edit billing?").unwrap(),
                    to: Addressees::Members(vec![Handle::parse("carol").unwrap()]),
                    item: None,
                    label: SessionLabel::parse("desktop/77aa01bc").unwrap(),
                    body: Body::parse("I need round.rs for an hour.").unwrap(),
                    importance: Importance::Normal,
                    ack_required: true,
                },
            )
            .await
            .unwrap();
        let thread = started.thread.record.id;
        let sent = escalate(&env, &carol.actor, EscalationSubject::Thread(thread))
            .await
            .unwrap();
        assert_eq!(
            handles(&sent.reached),
            ["bob", "dave"],
            "bob asked and was not answered; carol escalates and is not asked"
        );
        let outsider = escalate(&env, &env.alice.actor, EscalationSubject::Thread(thread))
            .await
            .unwrap_err();
        assert_eq!(
            outsider.code(),
            "not_found",
            "a conversation one may not read cannot be escalated"
        );
        let kinds = env.kinds_after(0).await;
        let escalated = kinds
            .iter()
            .find(|kind| matches!(kind, EventKind::Escalated(_)))
            .unwrap();
        assert_eq!(
            escalated.thread(),
            Some(thread),
            "only the conversation's readers see it"
        );

        let private = "May I edit billing?";
        let copy = ReadThread::new(&env.deps)
            .execute(&dave.actor, sent.thread)
            .await
            .unwrap();
        let subject = copy.thread.record.subject.to_string();
        let text = copy.messages[0].record.body.to_string();
        assert_eq!(
            subject,
            format!("Escalated by carol: conversation #{thread}")
        );
        assert!(
            !subject.contains(private) && !text.contains(private),
            "{text}"
        );
        assert!(
            text.contains(&format!("to settle conversation #{thread}")),
            "{text}"
        );
        assert_eq!(
            ReadThread::new(&env.deps)
                .execute(&dave.actor, thread)
                .await
                .unwrap_err()
                .code(),
            "not_found",
            "dave may not read the conversation itself"
        );
        let banner = Status::new(&env.deps)
            .execute(&dave.actor)
            .await
            .unwrap()
            .escalations;
        assert_eq!(banner[0].about, format!("conversation #{thread}"));
    }

    #[tokio::test]
    async fn the_board_shows_an_escalation_until_an_arbiter_acknowledges_it() {
        let (env, carol, dave) = project().await;
        let item = env.item("gh:acme/widgets#2").await;
        let sent = escalate(&env, &carol.actor, EscalationSubject::Item(item))
            .await
            .unwrap();
        let open = |who: Actor| {
            let env = &env;
            async move {
                Status::new(&env.deps)
                    .execute(&who)
                    .await
                    .unwrap()
                    .escalations
            }
        };
        let shown = open(dave.actor.clone()).await;
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].by.as_str(), "carol");
        assert_eq!(handles(&shown[0].arbiters), ["dave"]);
        assert_eq!(
            shown[0].about,
            "work item gh:acme/widgets#2 \"Work on gh:acme/widgets#2\""
        );
        assert_eq!(
            open(carol.actor.clone()).await.len(),
            1,
            "its writer sees it"
        );
        assert!(
            open(env.bob.actor.clone()).await.is_empty(),
            "a member it did not reach does not"
        );
        AckMessage::new(&env.deps)
            .execute(&carol.actor, sent.message)
            .await
            .unwrap_err();
        AckMessage::new(&env.deps)
            .execute(&dave.actor, sent.message)
            .await
            .unwrap();
        assert!(
            open(dave.actor.clone()).await.is_empty(),
            "the arbiter settled it"
        );
        assert!(open(carol.actor.clone()).await.is_empty());
        assert!(
            escalate(&env, &carol.actor, EscalationSubject::Item(item))
                .await
                .is_ok(),
            "once settled, the subject may be escalated again"
        );
    }

    #[tokio::test]
    async fn without_another_arbiter_nothing_is_escalated() {
        let (env, _carol, dave) = project().await;
        let item = env.item("gh:acme/widgets#3").await;
        let before = env.events().await.len();
        let refused = escalate(&env, &dave.actor, EscalationSubject::Item(item))
            .await
            .unwrap_err();
        assert_eq!(refused.code(), "no_arbiters");
        assert_eq!(env.events().await.len(), before);
        let plain = Env::new().await;
        let item = plain.item("gh:acme/widgets#3").await;
        let refused = escalate(&plain, &plain.bob.actor, EscalationSubject::Item(item))
            .await
            .unwrap_err();
        assert_eq!(refused.code(), "no_arbiters");
        assert_eq!(
            escalate(&plain, &plain.importer, EscalationSubject::Item(item))
                .await
                .unwrap_err()
                .code(),
            "forbidden"
        );
    }
}
