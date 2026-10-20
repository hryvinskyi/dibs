//! Removing a member from a project.

use std::fmt::Write as _;
use std::sync::Arc;

use dibs_domain::event::{self, EventKind, MemberRemoved};
use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::item::ItemStatus;
use dibs_domain::message::{Author, Importance, Recipients};
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_domain::slot::{RemovedFromSlots, SlotName, decide_member_removed};

use crate::authority::AdminAuthority;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{ItemName, Names};
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::records::MemberRecord;
use crate::use_cases::claim_ending::{Ending, end_claim};
use crate::use_cases::common::{existing_item, present_member_by_handle};
use crate::use_cases::slots::Scene;
use crate::use_cases::writing::{Posted, SystemPost, system_body};
use crate::work::Work;

/// A member removed, and what went with them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedMember {
    /// The member, as they are kept: marked removed.
    pub member: MemberRecord,
    /// The items whose claims were released, which are open again.
    pub released: Vec<ItemName>,
    /// What they had among the slots.
    pub slots: RemovedFromSlots,
    /// How many of their tokens were revoked.
    pub tokens_revoked: u64,
    /// How many of their browser sessions ended.
    pub ui_sessions_ended: u64,
}

/// Removes a member from a project, for an admin of it — never the admin themself — or for the
/// operator of the service, in one transaction: every token of theirs is revoked and every browser
/// session ended; each of their claims is released, its item open again (`force_released`); their
/// holds on slots end and their places in queues and the offers to them go, the freed slots
/// offered on (`slot_force_released`); they are marked removed (`member_removed`); and dibs tells
/// the project, in one message, what was released. Their messages, and every record that names
/// them, stay. A removed member is no recipient, takes nothing and signs in nowhere.
pub struct RemoveMember {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
    ref_schemes: Arc<RefSchemeRegistry>,
}

impl RemoveMember {
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

    /// Removes the member with this handle from the project `by` administers.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] unless `by` may administer the project; [`AppError::NotFound`] when
    /// the project has no such member, or removed them before; [`AppError::SelfRemoval`] when an
    /// admin names themself; [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        by: &impl AdminAuthority,
        handle: &Handle,
    ) -> Result<RemovedMember, AppError> {
        let project = by.administered_project()?;
        let admin = by.acting_member();
        let now = self.clock.now();
        let mut work = Work::begin_administering(&*self.store, by).await?;
        let member = present_member_by_handle(&mut work, handle).await?;
        if admin == Some(member.id) {
            return Err(AppError::SelfRemoval);
        }
        let mut tokens_revoked = 0;
        for token in work.tokens_of(project, member.id).await? {
            if token.is_active() {
                work.revoke_token(project, &token.hash, now).await?;
                tokens_revoked += 1;
            }
        }
        let ui_sessions_ended = work.delete_ui_sessions_of(project, member.id).await?;
        let mut released = Vec::new();
        for claim in work.claims(project).await? {
            if claim.member != member.id {
                continue;
            }
            let item = existing_item(&mut work, claim.item).await?;
            let ending = Ending {
                status: ItemStatus::Open,
                event: EventKind::ForceReleased(event::ForceReleased {
                    item: claim.item,
                    member: member.id,
                    by: admin,
                }),
            };
            end_claim(&mut work, &claim, ending, now).await?;
            released.push(ItemName::of(&item, &self.ref_schemes));
        }
        let mut scene = Scene::load(&mut work, &self.hot_rules, now).await?;
        let transition =
            decide_member_removed(&scene.board, scene.catalog(), member.id, admin, now);
        let slots = scene.apply(transition);
        scene.save(&mut work, &*self.ids, now).await?;
        work.remove_member(project, member.id, now).await?;
        work.record(
            now,
            EventKind::MemberRemoved(MemberRemoved {
                member: member.id,
                by: admin,
            }),
        )
        .await?;
        let names = Names::load(&mut work).await?;
        let who = match admin {
            Some(admin) => format!("{}, an admin,", names.handle(admin)?),
            None => "the operator of this service".to_owned(),
        };
        SystemPost {
            ids: Posted::next(&*self.ids),
            subject: format!("{} left the project", member.handle),
            item: None,
            on_behalf_of: admin.map_or(Author::System, Author::Member),
            to: Recipients::Project,
            body: system_body(&announcement(&member, &who, &released, &slots), None)?,
            importance: Importance::Normal,
            ack_required: false,
        }
        .send(&mut work, now)
        .await?;
        let member = work
            .member(project, member.id)
            .await?
            .unwrap_or(MemberRecord {
                removed_at: Some(now),
                ..member
            });
        work.commit(&*self.notifier).await?;
        Ok(RemovedMember {
            member,
            released,
            slots,
            tokens_revoked,
            ui_sessions_ended,
        })
    }
}

/// How many of each kind of thing released the message to the project names before it counts the
/// rest.
const LISTED: usize = 20;

/// What dibs tells the project: who was removed, by whom, and what was released — the first
/// [`LISTED`] of each kind by name, then how many more, so the message fits however much they held.
fn announcement(
    member: &MemberRecord,
    who: &str,
    released: &[ItemName],
    slots: &RemovedFromSlots,
) -> String {
    let mut text = format!(
        "{who} removed {} from this project. Their tokens no longer work; their messages stay.",
        member.handle
    );
    if released.is_empty() && slots.is_empty() {
        text.push_str("\nThey held nothing, so nothing was released.");
        return text;
    }
    if !released.is_empty() {
        let items = listed(released.iter().map(|item| {
            let label = item
                .external_ref
                .as_ref()
                .map_or_else(|| format!("#{}", item.id), ToString::to_string);
            format!("{label} \"{}\"", item.title)
        }));
        let _ = write!(
            text,
            "\nTheir claims were released; these work items are open again: {items}."
        );
    }
    let names = |slots: &[SlotName]| listed(slots.iter().map(ToString::to_string));
    if !slots.held.is_empty() {
        let _ = write!(
            text,
            "\nThe slots they held are free or offered on: {}.",
            names(&slots.held)
        );
    }
    if !slots.offered.is_empty() {
        let _ = write!(
            text,
            "\nThe slots offered to them passed on: {}.",
            names(&slots.offered)
        );
    }
    if !slots.waited.is_empty() {
        let _ = write!(text, "\nThey left the queues of: {}.", names(&slots.waited));
    }
    text
}

/// The first [`LISTED`] names joined, then how many more there are and where to see them.
fn listed(names: impl ExactSizeIterator<Item = String>) -> String {
    let total = names.len();
    let shown: Vec<String> = names.take(LISTED).collect();
    let mut text = shown.join(", ");
    if total > LISTED {
        let _ = write!(
            text,
            " and {} more (dibs status, or the board, shows what stands now)",
            total - LISTED
        );
    }
    text
}

#[cfg(test)]
mod tests {
    use dibs_domain::event::{ForceCause, SlotForceReleased};
    use dibs_domain::ids::ItemId;
    use dibs_domain::member::Role;
    use dibs_domain::message::{Body, SessionLabel, Subject};
    use dibs_domain::project::ProjectSlug;
    use dibs_domain::slot::Purpose;

    use super::*;
    use crate::actor::Actor;
    use crate::token::SecretToken;
    use crate::use_cases::testing::{Env, Person, new_member, session};
    use dibs_domain::item::{ExternalRef, Title};

    use crate::use_cases::{
        Addressees, Authenticate, ClaimItem, ClaimItemInput, CreateItem, CreateItemInput,
        CreateMember, CreateMemberInput, HoldMode, Inbox, InboxQuery, ListMembers, OpenUiSession,
        OperateProject, ReadThread, ResolveUiSession, StartThread, StartThreadInput, TakeSlot,
        TakeSlotInput,
    };

    const SETTINGS: &str = r#"{
        "areas": [{"name": "billing", "paths": ["packages/billing/**"]}],
        "slots": [
            {"name": "deploy:prod", "max_hold": "30m"},
            {"name": "db:staging", "max_hold": "1h"}
        ]
    }"#;

    /// A member `erin` whose token the test keeps, beside alice and bob.
    async fn project() -> (Env, Person, SecretToken) {
        let env = Env::with_settings(SETTINGS).await;
        let token = CreateMember::new(&env.deps)
            .execute(
                &env.alice.actor,
                CreateMemberInput {
                    member: new_member("erin"),
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
        let session = session(&env.deps, &actor, "laptop", "laptop/e1").await;
        (env, Person { actor, session }, token)
    }

    async fn take(env: &Env, who: &Person, slot: &str) {
        TakeSlot::new(&env.deps)
            .execute(
                &who.actor,
                TakeSlotInput {
                    slot: SlotName::parse(slot).unwrap(),
                    session: who.session,
                    purpose: Purpose::parse("ship").unwrap(),
                    item: None,
                    mode: HoldMode::Session,
                },
            )
            .await
            .unwrap();
    }

    async fn claim(env: &Env, who: &Person, item: ItemId) {
        ClaimItem::new(&env.deps)
            .execute(
                &who.actor,
                ClaimItemInput {
                    item,
                    session: who.session,
                    areas: vec!["billing".to_owned()],
                    wide: None,
                },
            )
            .await
            .unwrap();
    }

    fn erin() -> Handle {
        Handle::parse("erin").unwrap()
    }

    #[tokio::test]
    async fn removal_releases_claims_slots_and_queues_ends_sign_ins_and_keeps_messages() {
        let (env, erin_person, token) = project().await;
        let item = env.item("gh:acme/widgets#1").await;
        claim(&env, &erin_person, item).await;
        take(&env, &erin_person, "deploy:prod").await;
        take(&env, &env.bob, "deploy:prod").await;
        take(&env, &env.bob, "db:staging").await;
        take(&env, &erin_person, "db:staging").await;
        let browser = OpenUiSession::new(&env.deps)
            .execute(token.expose())
            .await
            .unwrap();
        let said = StartThread::new(&env.deps)
            .execute(
                &erin_person.actor,
                StartThreadInput {
                    subject: Subject::parse("Notes").unwrap(),
                    to: Addressees::Members(vec![Handle::parse("alice").unwrap()]),
                    item: None,
                    label: SessionLabel::parse("laptop/e1").unwrap(),
                    body: Body::parse("Handing over billing.").unwrap(),
                    importance: dibs_domain::message::Importance::Normal,
                    ack_required: false,
                },
            )
            .await
            .unwrap();
        let before = env.events().await.len();

        let removed = RemoveMember::new(&env.deps)
            .execute(&env.alice.actor, &erin())
            .await
            .unwrap();
        assert!(removed.member.removed_at.is_some());
        assert_eq!(removed.tokens_revoked, 1);
        assert_eq!(removed.ui_sessions_ended, 1);
        assert_eq!(
            removed
                .released
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            [item]
        );
        assert_eq!(
            removed.slots.held,
            [SlotName::parse("deploy:prod").unwrap()]
        );
        assert_eq!(
            removed.slots.waited,
            [SlotName::parse("db:staging").unwrap()]
        );

        let erin_id = erin_person.actor.member;
        let alice = Some(env.alice.actor.member);
        let kinds = env.kinds_after(before).await;
        assert_eq!(
            kinds[0],
            EventKind::ForceReleased(event::ForceReleased {
                item,
                member: erin_id,
                by: alice,
            })
        );
        let ended_slot = |slot: &str| {
            EventKind::SlotForceReleased(SlotForceReleased {
                slot: SlotName::parse(slot).unwrap(),
                member: erin_id,
                by: alice,
                cause: ForceCause::MemberRemoved,
            })
        };
        assert!(kinds.contains(&ended_slot("deploy:prod")));
        assert!(kinds.contains(&ended_slot("db:staging")));
        assert!(kinds.contains(&EventKind::MemberRemoved(MemberRemoved {
            member: erin_id,
            by: alice,
        })));
        let names: Vec<&str> = kinds.iter().map(EventKind::name).collect();
        assert_eq!(
            names
                .iter()
                .filter(|name| **name == "message_posted")
                .count(),
            1,
            "one message to the project: {names:?}"
        );
        assert!(
            names.contains(&"slot_offered"),
            "bob is offered deploy:prod"
        );

        let refused = Authenticate::new(&env.deps)
            .execute(token.expose())
            .await
            .unwrap_err();
        assert_eq!(refused.code(), "unauthorized");
        let late = env.item("gh:acme/widgets#2").await;
        let after = ClaimItem::new(&env.deps)
            .execute(
                &erin_person.actor,
                ClaimItemInput {
                    item: late,
                    session: erin_person.session,
                    areas: Vec::new(),
                    wide: None,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(
            after.code(),
            "unauthorized",
            "a request signed in before the removal holds nothing after it"
        );
        let slot_after = TakeSlot::new(&env.deps)
            .execute(
                &erin_person.actor,
                TakeSlotInput {
                    slot: SlotName::parse("deploy:prod").unwrap(),
                    session: erin_person.session,
                    purpose: Purpose::parse("again").unwrap(),
                    item: None,
                    mode: HoldMode::Session,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(slot_after.code(), "unauthorized");
        assert!(
            ResolveUiSession::new(&env.deps)
                .execute(browser.secret.expose())
                .await
                .is_err()
        );
        let mut tx = env.deps.store.begin().await.unwrap();
        let stored = tx.item(env.project, item).await.unwrap().unwrap();
        assert_eq!(stored.status, ItemStatus::Open);
        assert!(tx.claim(env.project, item).await.unwrap().is_none());
        let board = tx.slot_board(env.project).await.unwrap();
        assert!(board.holds.iter().all(|hold| hold.member != erin_id));
        assert!(board.waiters.iter().all(|waiter| waiter.member != erin_id));
        drop(tx);

        let members = ListMembers::new(&env.deps)
            .execute(&env.alice.actor)
            .await
            .unwrap();
        assert!(members.iter().all(|member| member.handle != erin()));
        let kept = ReadThread::new(&env.deps)
            .execute(&env.alice.actor, said.thread.record.id)
            .await
            .unwrap();
        assert_eq!(
            kept.messages[0].author.as_str(),
            "erin",
            "her messages stay"
        );

        let inbox = Inbox::new(&env.deps)
            .execute(&env.bob.actor, InboxQuery::default())
            .await
            .unwrap();
        let told = inbox
            .threads
            .iter()
            .find(|summary| summary.thread.record.subject.as_str() == "erin left the project")
            .unwrap();
        assert!(told.thread.record.recipients == Recipients::Project);
        let text = ReadThread::new(&env.deps)
            .execute(&env.bob.actor, told.thread.record.id)
            .await
            .unwrap()
            .messages[0]
            .record
            .body
            .to_string();
        assert!(
            text.starts_with("alice, an admin, removed erin from this project."),
            "{text}"
        );
        assert!(text.contains("gh:acme/widgets#1"), "{text}");
        assert!(text.contains("deploy:prod"), "{text}");
        assert!(
            text.contains("They left the queues of: db:staging."),
            "{text}"
        );

        let again = RemoveMember::new(&env.deps)
            .execute(&env.alice.actor, &erin())
            .await
            .unwrap_err();
        assert_eq!(again.code(), "not_found");
        let rotated = crate::use_cases::RotateToken::new(&env.deps)
            .execute(&env.alice.actor, &erin())
            .await
            .unwrap_err();
        assert_eq!(
            rotated.code(),
            "not_found",
            "a removed member gets no new token"
        );
        let readd = CreateMember::new(&env.deps)
            .execute(
                &env.alice.actor,
                CreateMemberInput {
                    member: new_member("erin"),
                    role: Role::Member,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(readd.code(), "handle_taken", "the history keeps her name");
    }

    #[tokio::test]
    async fn removing_a_member_who_held_many_claims_with_long_titles_succeeds() {
        let (env, erin_person, _token) = project().await;
        let title = "Ünïcödé títle ".repeat(14);
        for n in 0..120 {
            let item = CreateItem::new(&env.deps)
                .execute(
                    &env.alice.actor,
                    CreateItemInput {
                        external_ref: Some(
                            ExternalRef::parse(&format!("gh:acme/widgets#{}", 1000 + n)).unwrap(),
                        ),
                        title: Title::parse(&title).unwrap(),
                    },
                )
                .await
                .unwrap()
                .item
                .item
                .id;
            ClaimItem::new(&env.deps)
                .execute(
                    &erin_person.actor,
                    ClaimItemInput {
                        item,
                        session: erin_person.session,
                        areas: Vec::new(),
                        wide: None,
                    },
                )
                .await
                .unwrap();
        }
        let removed = RemoveMember::new(&env.deps)
            .execute(&env.alice.actor, &erin())
            .await
            .unwrap();
        assert_eq!(removed.released.len(), 120);
        let inbox = Inbox::new(&env.deps)
            .execute(&env.bob.actor, InboxQuery::default())
            .await
            .unwrap();
        let told = inbox
            .threads
            .iter()
            .find(|summary| summary.thread.record.subject.as_str() == "erin left the project")
            .unwrap();
        let text = ReadThread::new(&env.deps)
            .execute(&env.bob.actor, told.thread.record.id)
            .await
            .unwrap()
            .messages[0]
            .record
            .body
            .to_string();
        assert!(text.contains("and 100 more"), "{text}");
        assert_eq!(text.matches("gh:acme/widgets#").count(), 20, "{text}");
    }

    #[tokio::test]
    async fn only_another_admin_or_the_operator_removes_a_member() {
        let (env, _erin, _token) = project().await;
        let bob = RemoveMember::new(&env.deps)
            .execute(&env.bob.actor, &erin())
            .await
            .unwrap_err();
        assert_eq!(bob.code(), "forbidden");
        let alice = RemoveMember::new(&env.deps)
            .execute(&env.alice.actor, &Handle::parse("alice").unwrap())
            .await
            .unwrap_err();
        assert_eq!(alice.code(), "self_removal");
        let nobody = RemoveMember::new(&env.deps)
            .execute(&env.alice.actor, &Handle::parse("nobody").unwrap())
            .await
            .unwrap_err();
        assert_eq!(nobody.code(), "not_found");

        let operator = OperateProject::new(&env.deps)
            .execute(&ProjectSlug::parse("widgets").unwrap())
            .await
            .unwrap();
        let before = env.events().await.len();
        RemoveMember::new(&env.deps)
            .execute(&operator, &erin())
            .await
            .unwrap();
        let kinds = env.kinds_after(before).await;
        assert!(kinds.iter().any(|kind| matches!(
            kind,
            EventKind::MemberRemoved(MemberRemoved { by: None, .. })
        )));
        assert!(kinds.iter().any(|kind| matches!(
            kind,
            EventKind::ThreadStarted(event::ThreadStarted { author: None, .. })
        )));
    }

    #[tokio::test]
    async fn a_removal_that_fails_to_commit_changes_nothing() {
        let (env, erin_person, token) = project().await;
        let item = env.item("gh:acme/widgets#4").await;
        claim(&env, &erin_person, item).await;
        let before = env.events().await.len();
        env.fakes.store.fail_next_commit();
        assert!(
            RemoveMember::new(&env.deps)
                .execute(&env.alice.actor, &erin())
                .await
                .is_err()
        );
        assert_eq!(env.events().await.len(), before);
        let still: Actor = Authenticate::new(&env.deps)
            .execute(token.expose())
            .await
            .unwrap();
        assert_eq!(still.member, erin_person.actor.member);
        let mut tx = env.deps.store.begin().await.unwrap();
        assert!(tx.claim(env.project, item).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn every_request_signed_in_before_a_removal_is_refused_after_it() {
        use dibs_domain::ids::{MessageId, ThreadId};
        use dibs_domain::session::{AgentLabel, HostName};

        use crate::use_cases::{
            AckMessage, DeleteThread, MarkRead, PostMessage, PostMessageInput, RegisterSession,
            RegisterSessionInput, Status,
        };

        let (env, erin_person, _token) = project().await;
        let dave = {
            let token = CreateMember::new(&env.deps)
                .execute(
                    &env.alice.actor,
                    CreateMemberInput {
                        member: new_member("dave"),
                        role: Role::Admin,
                    },
                )
                .await
                .unwrap()
                .token;
            Authenticate::new(&env.deps)
                .execute(token.expose())
                .await
                .unwrap()
        };
        let thread = StartThread::new(&env.deps)
            .execute(
                &env.alice.actor,
                StartThreadInput {
                    subject: Subject::parse("Rates").unwrap(),
                    to: Addressees::Project,
                    item: None,
                    label: SessionLabel::parse("web").unwrap(),
                    body: Body::parse("Where is the table?").unwrap(),
                    importance: dibs_domain::message::Importance::Normal,
                    ack_required: true,
                },
            )
            .await
            .unwrap();
        let remove = RemoveMember::new(&env.deps);
        remove.execute(&env.alice.actor, &erin()).await.unwrap();
        remove
            .execute(&env.alice.actor, &Handle::parse("dave").unwrap())
            .await
            .unwrap();
        let before = env.events().await.len();
        let erin = &erin_person.actor;
        let thread_id: ThreadId = thread.thread.record.id;
        let message: MessageId = thread.message.record.id;
        let refused = vec![
            PostMessage::new(&env.deps)
                .execute(
                    erin,
                    PostMessageInput {
                        thread: thread_id,
                        label: SessionLabel::parse("web").unwrap(),
                        body: Body::parse("still here").unwrap(),
                        importance: dibs_domain::message::Importance::Normal,
                        ack_required: false,
                    },
                )
                .await
                .map(|_| ()),
            StartThread::new(&env.deps)
                .execute(
                    erin,
                    StartThreadInput {
                        subject: Subject::parse("Late").unwrap(),
                        to: Addressees::Project,
                        item: None,
                        label: SessionLabel::parse("web").unwrap(),
                        body: Body::parse("hi").unwrap(),
                        importance: dibs_domain::message::Importance::Normal,
                        ack_required: false,
                    },
                )
                .await
                .map(|_| ()),
            AckMessage::new(&env.deps)
                .execute(erin, message)
                .await
                .map(|_| ()),
            MarkRead::new(&env.deps)
                .execute(erin, thread_id)
                .await
                .map(|_| ()),
            RegisterSession::new(&env.deps)
                .execute(
                    erin,
                    RegisterSessionInput {
                        host: HostName::parse("laptop").unwrap(),
                        label: SessionLabel::parse("laptop/late").unwrap(),
                        agent: AgentLabel::parse("agent").unwrap(),
                        push: false,
                    },
                )
                .await
                .map(|_| ()),
            Status::new(&env.deps).execute(erin).await.map(|_| ()),
            remove
                .execute(&dave, &Handle::parse("bob").unwrap())
                .await
                .map(|_| ()),
            DeleteThread::new(&env.deps)
                .execute(&dave, thread_id)
                .await
                .map(|_| ()),
        ];
        for (n, answer) in refused.into_iter().enumerate() {
            assert_eq!(answer.unwrap_err().code(), "unauthorized", "case {n}");
        }
        assert_eq!(
            env.events().await.len(),
            before,
            "nothing was done in their names"
        );
    }
}
