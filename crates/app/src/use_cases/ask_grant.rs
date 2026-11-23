//! A member asking whoever holds an area for a grant, in a conversation that reaches the session
//! working under the holder's claim.

use std::sync::Arc;

use time::Duration;

use dibs_domain::area::QualifiedArea;
use dibs_domain::grant::{
    AskOutcome, AskedGrant, GrantAsk, GrantDecision, OnBehalf, decide_grant_ask,
};
use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{GrantId, ItemId, ThreadId};
use dibs_domain::message::{Importance, Recipients};
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_domain::settings::format_duration;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{ItemName, Names};
use crate::ports::{Clock, EventNotifier, IdGen, Store, StoreError};
use crate::use_cases::common::existing_item;
use crate::use_cases::on_behalf::OnBehalfOf;
use crate::use_cases::writing::{MemberThread, Posted, fit_subject, titled_body};
use crate::work::Work;

/// The grant to ask for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskGrantInput {
    /// The caller's work item: the work that needs the edit.
    pub item: ItemId,
    /// The area, as a person names it.
    pub area: String,
}

/// What asking came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantAsked {
    /// The holder was asked, in a new conversation about the item of their claim.
    Asked {
        /// The holder.
        holder: Handle,
        /// The item of the holder's claim that holds the area.
        holder_item: ItemName,
        /// The conversation that asks them.
        thread: ThreadId,
        /// Whether dibs left the request to the holder for good: a revoke of the holder's grant to
        /// the caller in the area stands while both claims last, so dibs never grants it on the
        /// holder's behalf, and only the holder grants or answers it.
        left_to_holder: bool,
    },
    /// No live claim holds the area; the caller can claim it.
    Free,
    /// A live claim of the caller's own holds the area.
    Own,
    /// The holder's grant to the caller in the area is in force: given before, or given now by
    /// dibs on the holder's behalf.
    Granted {
        /// The holder whose grant it is.
        holder: Handle,
        /// The item of the holder's claim that holds the area.
        holder_item: ItemName,
        /// The grant dibs gave now, and why; `None` when the holder's grant was in force already.
        by_dibs: Option<GrantedByDibs>,
    },
}

/// A grant dibs gave on the holder's behalf the moment it was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrantedByDibs {
    /// The grant.
    pub grant: GrantId,
    /// Why dibs gave it.
    pub decided: GrantDecision,
    /// The conversation that asked the holder, where dibs said so.
    pub thread: ThreadId,
}

/// The answer to asking for a grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskGrantResult {
    /// What asking came to.
    pub outcome: GrantAsked,
    /// The caller's item.
    pub item: ItemName,
    /// The area, resolved.
    pub area: QualifiedArea,
}

/// Asks the member whose live claim holds an area for a grant there, for the caller's live claim
/// on an item. The conversation is about the holder's item and addressed to the holder alone, so
/// it reaches the session working under the claim that holds the area. The request is recorded
/// with its first message, so the holder's inbox lists it until it is decided.
///
/// dibs decides the request on the holder's behalf in the same transaction, from what it stores,
/// before the first message is written, and the message says what came of it:
/// - when the holder's claim is in review, the area is idle on it, or its branch changes no path of
///   the area, dibs gives the holder's grant at once, settles the request and says so in the
///   conversation. The first message, in the caller's name, then says what was asked and that dibs
///   granted it; it is normal and counts toward no limit, as nothing is left for the holder to do;
/// - when a revoke of the holder's grant to the caller in the area stands, dibs leaves the request
///   to the holder for good and never judges it again. The first message is high, says how to give
///   the grant, and says that dibs leaves it to the holder;
/// - otherwise the request waits, for the holder or for the sweep, which grants it once the
///   project's `grant_after` has passed. The first message is high, says how to give the grant,
///   and when dibs grants it on the holder's behalf.
///
/// A high first message asks for no acknowledgement and is held to the caller's limit on raised
/// messages an hour.
pub struct AskGrant {
    ref_schemes: Arc<RefSchemeRegistry>,
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
}

impl AskGrant {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            ref_schemes: deps.ref_schemes.clone(),
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            ids: deps.ids.clone(),
            notifier: deps.notifier.clone(),
            hot_rules: deps.hot_rules.clone(),
        }
    }

    /// Asks for the grant.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] for an unknown item;
    /// [`AppError::Area`] for an area that does not resolve; [`AppError::AskGrant`] when the caller
    /// holds no live claim on the item; [`AppError::ImportanceLimited`] when the request is asked
    /// in a high message and the caller already wrote as many high or urgent messages within the
    /// hour as the project allows;
    /// [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        input: AskGrantInput,
    ) -> Result<AskGrantResult, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let project = actor.project;
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let item = existing_item(&mut work, input.item).await?;
        let settings = work.project_settings(&self.hot_rules).await?;
        let ask = GrantAsk {
            asker: actor.member,
            item: item.id,
            area: settings.resolve_area(&input.area)?,
        };
        let records = work.grants(project).await?;
        let grants: Vec<_> = records.iter().map(|record| record.grant.clone()).collect();
        let claims = work.claims(project).await?;
        let item_name = ItemName::of(&item, &self.ref_schemes);
        let outcome = match decide_grant_ask(&ask, &grants, &claims, now, &settings)? {
            AskOutcome::Free => GrantAsked::Free,
            AskOutcome::Own => GrantAsked::Own,
            AskOutcome::AlreadyGranted {
                holder,
                holder_item,
            } => GrantAsked::Granted {
                holder: Names::load(&mut work).await?.handle(holder)?,
                holder_item: ItemName::of(
                    &existing_item(&mut work, holder_item).await?,
                    &self.ref_schemes,
                ),
                by_dibs: None,
            },
            AskOutcome::Ask {
                holder,
                holder_item,
            } => {
                let holder_name = ItemName::of(
                    &existing_item(&mut work, holder_item).await?,
                    &self.ref_schemes,
                );
                let names = Names::load(&mut work).await?;
                let asker = names.handle(actor.member)?;
                let session = claims
                    .iter()
                    .find(|claim| claim.member == actor.member && claim.item == item.id)
                    .map(|claim| claim.session)
                    .ok_or_else(|| corrupt("the claim the domain found is not among the claims"))?;
                let label = work
                    .session(project, session)
                    .await?
                    .ok_or_else(|| corrupt("the session of a live claim is not stored"))?
                    .label;
                let posted = Posted::next(&*self.ids);
                let asked = AskedGrant {
                    message: posted.message,
                    ask: ask.clone(),
                    holder,
                };
                let blocks = work.grant_blocks(project).await?;
                let listed = work.footprints(project).await?;
                let behalf = OnBehalfOf {
                    claims: &claims,
                    blocks: &blocks,
                    listed: &listed,
                    settings: &settings,
                    ids: &*self.ids,
                    schemes: &self.ref_schemes,
                    now,
                };
                let decided = behalf.decide(&mut work, &asked, now, &records).await?;
                let fate = Asking::of(decided, settings.grant_after());
                let text = AskingText {
                    asker: &asker,
                    item: &item_name,
                    area: &ask.area,
                    holder_item: &holder_name,
                    fate,
                };
                let (thread, _) = MemberThread {
                    ids: posted,
                    subject: fit_subject(&text.subject())?,
                    item: Some(holder_item),
                    author: actor.member,
                    to: Recipients::members([holder].into())?,
                    label,
                    body: titled_body(&text.lead(), None)?,
                    importance: fate.importance(),
                    ack_required: false,
                }
                .start(&mut work, settings.raised_per_hour(), now)
                .await?;
                work.insert_asked_grant(project, &asked).await?;
                let holder = names.handle(holder)?;
                match decided {
                    OnBehalf::Grant { because, .. } => {
                        let given = behalf
                            .grant(&mut work, &asked, now, because, thread.id, &records)
                            .await?
                            .ok_or_else(|| {
                                corrupt("the grant dibs decided to give at the ask was not given")
                            })?;
                        GrantAsked::Granted {
                            holder,
                            holder_item: holder_name,
                            by_dibs: Some(GrantedByDibs {
                                grant: given.record.id,
                                decided: because,
                                thread: thread.id,
                            }),
                        }
                    }
                    OnBehalf::LeftToHolder => {
                        work.leave_asked_grant_to_holder(project, asked.message)
                            .await?;
                        GrantAsked::Asked {
                            holder,
                            holder_item: holder_name,
                            thread: thread.id,
                            left_to_holder: true,
                        }
                    }
                    OnBehalf::Closed => {
                        work.settle_asked_grant(project, asked.message, now).await?;
                        GrantAsked::Asked {
                            holder,
                            holder_item: holder_name,
                            thread: thread.id,
                            left_to_holder: false,
                        }
                    }
                    OnBehalf::Wait { .. } | OnBehalf::Lapsed => GrantAsked::Asked {
                        holder,
                        holder_item: holder_name,
                        thread: thread.id,
                        left_to_holder: false,
                    },
                }
            }
        };
        work.commit(&*self.notifier).await?;
        Ok(AskGrantResult {
            outcome,
            item: item_name,
            area: ask.area,
        })
    }
}

/// What the first message of a request says dibs does with it, decided before it is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Asking {
    /// dibs may grant it on the holder's behalf later: when its rules let it, and otherwise once
    /// this long, the project's `grant_after`, has passed since it was asked.
    Waits(Duration),
    /// A revoke of the holder's grant to the asker in the area stands: dibs leaves the request to
    /// the holder.
    LeftToHolder,
    /// dibs granted it on the holder's behalf as it was asked.
    GrantedAtOnce,
}

impl Asking {
    /// What dibs's decision on the request, `decided`, makes the first message say; `grant_after`
    /// is the project's.
    const fn of(decided: OnBehalf, grant_after: Duration) -> Self {
        match decided {
            OnBehalf::Grant { .. } => Self::GrantedAtOnce,
            OnBehalf::LeftToHolder => Self::LeftToHolder,
            OnBehalf::Wait { .. } | OnBehalf::Lapsed | OnBehalf::Closed => Self::Waits(grant_after),
        }
    }

    /// How much the first message asks for the holder's attention: normal once dibs granted the
    /// request, as nothing is left for the holder to do, and high while the holder may still give
    /// the grant.
    const fn importance(self) -> Importance {
        match self {
            Self::GrantedAtOnce => Importance::Normal,
            Self::Waits(_) | Self::LeftToHolder => Importance::High,
        }
    }
}

/// The words of a request for a grant, composed in one place.
struct AskingText<'a> {
    /// Who asks.
    asker: &'a Handle,
    /// The asker's item, the grant's item.
    item: &'a ItemName,
    /// The area.
    area: &'a QualifiedArea,
    /// The holder's item, whose claim holds the area.
    holder_item: &'a ItemName,
    /// What dibs does with the request.
    fate: Asking,
}

impl AskingText<'_> {
    /// The conversation's subject: the area, and the asker's item by its reference, else its
    /// title.
    fn subject(&self) -> String {
        let item = self
            .item
            .external_ref
            .as_ref()
            .map_or_else(|| self.item.title.to_string(), ToString::to_string);
        format!("A grant on {} for {item}", self.area)
    }

    /// The first message: who asks, for which item, and which claim holds the area. Once dibs
    /// granted it, that dibs did so on the holder's behalf. Otherwise how to give the grant, and
    /// either when dibs gives it on the holder's behalf and what the holder may do until then, or
    /// that a revoke of the holder's leaves it to them.
    fn lead(&self) -> String {
        let asker = self.asker;
        let area = self.area;
        let key = self.item.key();
        let holder_item = self.holder_item.key();
        let title = &self.item.title;
        let needs = format!(
            "their work item {key} (\"{title}\") needs edits in {area}, which your claim on work item {holder_item} holds."
        );
        let give = format!(
            "To give it: dibs grant {key} --area {area} --to {asker} (MCP: dibs_grant with key {key}, area {area}, to {asker}). It lasts while both claims do; you can revoke it. dibs tells {asker} when it is given."
        );
        match self.fate {
            Asking::GrantedAtOnce => format!(
                "{asker} asked you for a grant: {needs}\n\
                 dibs granted it at once on your behalf; its reply in this conversation says why and how to undo it."
            ),
            Asking::LeftToHolder => format!(
                "{asker} asks you for a grant: {needs}\n\
                 {give}\n\
                 A revoke of your grant to {asker} in {area} stands while both claims last, so dibs leaves this request to you to grant or answer: it does not grant it on your behalf. You may grant it, or answer in this conversation what your work still changes there."
            ),
            Asking::Waits(grant_after) => format!(
                "{asker} asks you for a grant: {needs}\n\
                 {give}\n\
                 dibs grants it on your behalf at once when your claim's branch changes nothing in {area}, the area is idle on your claim, or your claim is in review, and otherwise once {grant_after} have passed since this request (the project's grant_after). Until then you may grant it, or answer in this conversation what your work still changes there.",
                grant_after = format_duration(grant_after),
            ),
        }
    }
}

fn corrupt(what: &str) -> AppError {
    AppError::Store(StoreError::Corrupt {
        what: what.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use dibs_domain::message::Author;

    use super::*;
    use crate::use_cases::testing::{Env, Person};
    use crate::use_cases::{
        ClaimItem, ClaimItemInput, Grant, GrantInput, ReadThread, StartThread, StartThreadInput,
    };

    async fn claim(env: &Env, who: &Person, item: ItemId, areas: &[&str]) {
        ClaimItem::new(&env.deps)
            .execute(
                &who.actor,
                ClaimItemInput {
                    branches: Default::default(),
                    wide: None,
                    item,
                    session: who.session,
                    areas: areas.iter().map(|a| (*a).to_owned()).collect(),
                    kind: None,
                },
            )
            .await
            .unwrap();
    }

    fn billing(item: ItemId) -> AskGrantInput {
        AskGrantInput {
            item,
            area: "billing".to_owned(),
        }
    }

    /// The requests recorded as put to `holder` and not answered.
    async fn recorded(env: &Env, holder: &Person) -> Vec<AskedGrant> {
        env.deps
            .store
            .begin()
            .await
            .unwrap()
            .unanswered_asked_grants(env.project, holder.actor.member)
            .await
            .unwrap()
            .into_iter()
            .map(|record| record.asked)
            .collect()
    }

    /// alice holds billing on `#1`; bob claims `#2` with no area.
    async fn held() -> (Env, ItemId, ItemId) {
        let env = Env::new().await;
        let alices = env.item("gh:acme/widgets#1").await;
        let bobs = env.item("gh:acme/widgets#2").await;
        claim(&env, &env.alice, alices, &["billing"]).await;
        claim(&env, &env.bob, bobs, &[]).await;
        (env, alices, bobs)
    }

    #[tokio::test]
    async fn the_holder_is_asked_in_a_conversation_about_their_own_item() {
        let (env, alices, bobs) = held().await;
        let asked = AskGrant::new(&env.deps)
            .execute(&env.bob.actor, billing(bobs))
            .await
            .unwrap();
        let GrantAsked::Asked {
            holder,
            holder_item,
            thread,
            left_to_holder: false,
        } = asked.outcome
        else {
            panic!("not asked: {asked:?}");
        };
        assert_eq!(holder.as_str(), "alice");
        assert_eq!(holder_item.id, alices);
        assert_eq!(asked.item.id, bobs);
        assert_eq!(asked.area.to_string(), "default:billing");

        let view = ReadThread::new(&env.deps)
            .execute(&env.alice.actor, thread)
            .await
            .unwrap();
        let record = &view.thread.record;
        assert_eq!(record.item, Some(alices), "routed by the holder's item");
        assert_eq!(
            record.recipients,
            Recipients::Members([env.alice.actor.member].into())
        );
        assert_eq!(record.author, Author::Member(env.bob.actor.member));
        assert_eq!(
            record.subject.as_str(),
            "A grant on default:billing for gh:acme/widgets#2"
        );
        let [first] = &view.messages[..] else {
            panic!("one message: {:?}", view.messages);
        };
        let message = &first.record;
        assert_eq!(
            recorded(&env, &env.alice).await,
            [AskedGrant {
                message: message.id,
                ask: GrantAsk {
                    asker: env.bob.actor.member,
                    item: bobs,
                    area: asked.area.clone(),
                },
                holder: env.alice.actor.member,
            }],
            "the request is recorded with its message"
        );
        assert_eq!(message.author, Author::Member(env.bob.actor.member));
        assert_eq!(message.importance, Importance::High);
        assert!(!message.ack_required);
        assert_eq!(message.label.as_str(), "desktop/77aa01bc");
        let body = message.body.as_str();
        for part in [
            "bob asks you for a grant",
            "gh:acme/widgets#2 (\"Work on gh:acme/widgets#2\")",
            "your claim on work item gh:acme/widgets#1 holds",
            "dibs grant gh:acme/widgets#2 --area default:billing --to bob",
            "dibs_grant",
            "answer in this conversation",
        ] {
            assert!(body.contains(part), "{part}: {body}");
        }
    }

    #[tokio::test]
    async fn a_local_item_is_named_by_its_id_in_the_command() {
        let env = Env::new().await;
        let alices = env.item("gh:acme/widgets#1").await;
        claim(&env, &env.alice, alices, &["billing"]).await;
        let bobs = crate::use_cases::CreateItem::new(&env.deps)
            .execute(
                &env.bob.actor,
                crate::use_cases::CreateItemInput {
                    external_ref: None,
                    title: dibs_domain::item::Title::parse("Refund rounding").unwrap(),
                },
            )
            .await
            .unwrap()
            .item
            .item
            .id;
        claim(&env, &env.bob, bobs, &[]).await;
        let asked = AskGrant::new(&env.deps)
            .execute(&env.bob.actor, billing(bobs))
            .await
            .unwrap();
        let GrantAsked::Asked { thread, .. } = asked.outcome else {
            panic!("not asked: {asked:?}");
        };
        let view = ReadThread::new(&env.deps)
            .execute(&env.alice.actor, thread)
            .await
            .unwrap();
        assert_eq!(
            view.thread.record.subject.as_str(),
            "A grant on default:billing for Refund rounding"
        );
        let body = view.messages[0].record.body.as_str();
        assert!(
            body.contains(&format!(
                "dibs grant {bobs} --area default:billing --to bob"
            )),
            "{body}"
        );
    }

    #[tokio::test]
    async fn nobody_is_asked_for_a_free_area_or_ones_own() {
        let (env, _, bobs) = held().await;
        let ask = AskGrant::new(&env.deps);
        let before = env.events().await.len();
        let free = ask
            .execute(
                &env.bob.actor,
                AskGrantInput {
                    item: bobs,
                    area: "search".to_owned(),
                },
            )
            .await
            .unwrap();
        assert_eq!(free.outcome, GrantAsked::Free);
        assert_eq!(free.area.to_string(), "default:search");
        claim(&env, &env.bob, bobs, &["search"]).await;
        let after_claim = env.events().await.len();
        let own = ask
            .execute(
                &env.bob.actor,
                AskGrantInput {
                    item: bobs,
                    area: "search".to_owned(),
                },
            )
            .await
            .unwrap();
        assert_eq!(own.outcome, GrantAsked::Own);
        assert_eq!(env.events().await.len(), after_claim);
        assert!(after_claim > before);
        assert!(recorded(&env, &env.alice).await.is_empty());
    }

    #[tokio::test]
    async fn a_grant_in_force_is_not_asked_for_again() {
        let (env, alices, bobs) = held().await;
        Grant::new(&env.deps)
            .execute(
                &env.alice.actor,
                GrantInput {
                    item: bobs,
                    area: "billing".to_owned(),
                    to: Handle::parse("bob").unwrap(),
                },
            )
            .await
            .unwrap();
        let before = env.events().await.len();
        let granted = AskGrant::new(&env.deps)
            .execute(&env.bob.actor, billing(bobs))
            .await
            .unwrap();
        let GrantAsked::Granted {
            holder,
            holder_item,
            by_dibs,
        } = granted.outcome
        else {
            panic!("not granted: {granted:?}");
        };
        assert_eq!(holder.as_str(), "alice");
        assert_eq!(holder_item.id, alices);
        assert_eq!(by_dibs, None, "the holder's own grant was in force");
        assert_eq!(env.events().await.len(), before);
        assert!(recorded(&env, &env.alice).await.is_empty());
    }

    #[tokio::test]
    async fn asking_needs_a_live_claim_on_the_item() {
        let (env, alices, bobs) = held().await;
        let nobodys = env.item("gh:acme/widgets#3").await;
        let ask = AskGrant::new(&env.deps);
        let before = env.events().await.len();
        let cases = [
            (
                env.bob.actor.clone(),
                billing(nobodys),
                "grantee_has_no_claim",
            ),
            (
                env.bob.actor.clone(),
                billing(alices),
                "grantee_has_no_claim",
            ),
            (env.bob.actor.clone(), billing(ItemId(9999)), "not_found"),
            (
                env.bob.actor.clone(),
                AskGrantInput {
                    item: bobs,
                    area: "nowhere".to_owned(),
                },
                "unknown_area",
            ),
            (env.importer.clone(), billing(bobs), "forbidden"),
        ];
        for (actor, input, code) in cases {
            let error = ask.execute(&actor, input).await.unwrap_err();
            assert_eq!(error.code(), code);
        }
        assert_eq!(env.events().await.len(), before);
        env.advance(Duration::hours(25));
        let stale = ask
            .execute(&env.bob.actor, billing(bobs))
            .await
            .unwrap_err();
        assert_eq!(stale.code(), "grantee_has_no_claim");
    }

    #[tokio::test]
    async fn the_limit_on_high_messages_holds_for_an_ask() {
        let (env, _, bobs) = held().await;
        let start = StartThread::new(&env.deps);
        let alice = Handle::parse("alice").unwrap();
        let limit = 10;
        for n in 0..limit {
            start
                .execute(
                    &env.bob.actor,
                    StartThreadInput {
                        subject: dibs_domain::message::Subject::parse(&format!("Note {n}"))
                            .unwrap(),
                        to: crate::use_cases::Addressees::Members(vec![alice.clone()]),
                        item: None,
                        label: dibs_domain::message::SessionLabel::parse("desktop/77aa01bc")
                            .unwrap(),
                        body: dibs_domain::message::Body::parse("hello").unwrap(),
                        importance: Importance::High,
                        ack_required: false,
                    },
                )
                .await
                .unwrap();
        }
        let before = env.events().await.len();
        let limited = AskGrant::new(&env.deps)
            .execute(&env.bob.actor, billing(bobs))
            .await
            .unwrap_err();
        assert_eq!(limited.code(), "importance_limited");
        assert_eq!(env.events().await.len(), before, "nothing is written");
        assert!(recorded(&env, &env.alice).await.is_empty());
    }

    #[tokio::test]
    async fn an_ask_dibs_grants_at_once_uses_none_of_the_askers_raised_messages() {
        let (env, alices, bobs) = held().await;
        crate::use_cases::StartReview::new(&env.deps)
            .execute(
                &env.alice.actor,
                crate::use_cases::StartReviewInput {
                    head: None,
                    item: alices,
                    pr: dibs_domain::review::PrRef::parse("acme/widgets#7").unwrap(),
                    more: false,
                },
            )
            .await
            .unwrap();
        let start = StartThread::new(&env.deps);
        let alice = Handle::parse("alice").unwrap();
        for n in 0..10 {
            start
                .execute(
                    &env.bob.actor,
                    StartThreadInput {
                        subject: dibs_domain::message::Subject::parse(&format!("Note {n}"))
                            .unwrap(),
                        to: crate::use_cases::Addressees::Members(vec![alice.clone()]),
                        item: None,
                        label: dibs_domain::message::SessionLabel::parse("desktop/77aa01bc")
                            .unwrap(),
                        body: dibs_domain::message::Body::parse("hello").unwrap(),
                        importance: Importance::High,
                        ack_required: false,
                    },
                )
                .await
                .unwrap();
        }
        let raised = |env: &Env| {
            let store = env.deps.store.clone();
            let (project, bob, since) = (
                env.project,
                env.bob.actor.member,
                env.now() - Duration::hours(1),
            );
            async move {
                store
                    .begin()
                    .await
                    .unwrap()
                    .raised_since(project, bob, since)
                    .await
                    .unwrap()
                    .len()
            }
        };
        assert_eq!(
            raised(&env).await,
            10,
            "bob used every raised message of the hour"
        );

        let asked = AskGrant::new(&env.deps)
            .execute(&env.bob.actor, billing(bobs))
            .await
            .unwrap();

        let GrantAsked::Granted {
            by_dibs: Some(given),
            ..
        } = asked.outcome
        else {
            panic!("not granted by dibs: {asked:?}");
        };
        assert_eq!(raised(&env).await, 10, "the ask counts toward no limit");
        let view = ReadThread::new(&env.deps)
            .execute(&env.alice.actor, given.thread)
            .await
            .unwrap();
        let first = &view.messages[0].record;
        assert_eq!(first.author, Author::Member(env.bob.actor.member));
        assert_eq!(first.importance, Importance::Normal);
        let body = first.body.as_str();
        assert!(
            body.contains("dibs granted it at once on your behalf"),
            "{body}"
        );
        assert!(!body.contains("Until then"), "{body}");
        assert!(recorded(&env, &env.alice).await.is_empty(), "settled");
    }

    #[tokio::test]
    async fn a_grant_at_the_ask_records_when_the_oldest_request_it_settles_was_asked() {
        let (env, alices, bobs) = held().await;
        let ask = AskGrant::new(&env.deps);
        let first = env.now();
        let waiting = ask.execute(&env.bob.actor, billing(bobs)).await.unwrap();
        assert!(
            matches!(waiting.outcome, GrantAsked::Asked { .. }),
            "{waiting:?}"
        );
        env.advance(Duration::minutes(8));
        crate::use_cases::StartReview::new(&env.deps)
            .execute(
                &env.alice.actor,
                crate::use_cases::StartReviewInput {
                    head: None,
                    item: alices,
                    pr: dibs_domain::review::PrRef::parse("acme/widgets#7").unwrap(),
                    more: false,
                },
            )
            .await
            .unwrap();

        let again = ask.execute(&env.bob.actor, billing(bobs)).await.unwrap();

        assert!(
            matches!(
                again.outcome,
                GrantAsked::Granted {
                    by_dibs: Some(_),
                    ..
                }
            ),
            "{again:?}"
        );
        let granted: Vec<Option<time::OffsetDateTime>> = env
            .events()
            .await
            .into_iter()
            .filter_map(|event| match event.kind {
                dibs_domain::event::EventKind::Granted(granted) => Some(granted.asked_at),
                _ => None,
            })
            .collect();
        assert_eq!(granted, [Some(first)], "the first request's wait counts");
        assert!(recorded(&env, &env.alice).await.is_empty(), "both settled");
    }

    #[test]
    fn the_request_names_both_items_the_area_and_the_command() {
        let item = |id: u64, reference: Option<&str>, title: &str| ItemName {
            id: ItemId(id),
            external_ref: reference
                .map(|text| dibs_domain::item::ExternalRef::parse(text).unwrap()),
            title: dibs_domain::item::Title::parse(title).unwrap(),
            link: None,
        };
        let mine = item(7, None, "Refund rounding");
        let theirs = item(3, Some("gh:acme/widgets#42"), "Invoices");
        let area = QualifiedArea::parse("api:billing").unwrap();
        let asker = Handle::parse("bob").unwrap();
        let text = AskingText {
            asker: &asker,
            item: &mine,
            area: &area,
            holder_item: &theirs,
            fate: Asking::Waits(Duration::minutes(10)),
        };
        assert_eq!(text.subject(), "A grant on api:billing for Refund rounding");
        let lead = text.lead();
        for part in [
            "bob asks you for a grant: their work item 7 (\"Refund rounding\") needs edits in api:billing",
            "your claim on work item gh:acme/widgets#42 holds",
            "dibs grant 7 --area api:billing --to bob",
            "MCP: dibs_grant with key 7, area api:billing, to bob",
            "dibs grants it on your behalf at once when your claim's branch changes nothing in api:billing, the area is idle on your claim, or your claim is in review, and otherwise once 10m have passed since this request (the project's grant_after).",
            "Until then you may grant it, or answer in this conversation what your work still changes there.",
        ] {
            assert!(lead.contains(part), "{part}: {lead}");
        }
        assert!(
            !lead.contains("Nothing is granted until you give it"),
            "dibs may grant it: {lead}"
        );
    }

    #[test]
    fn a_request_left_to_the_holder_says_so_and_promises_no_grant() {
        let mine = ItemName {
            id: ItemId(7),
            external_ref: None,
            title: dibs_domain::item::Title::parse("Refund rounding").unwrap(),
            link: None,
        };
        let theirs = ItemName {
            id: ItemId(3),
            external_ref: Some(
                dibs_domain::item::ExternalRef::parse("gh:acme/widgets#42").unwrap(),
            ),
            title: dibs_domain::item::Title::parse("Invoices").unwrap(),
            link: None,
        };
        let area = QualifiedArea::parse("api:billing").unwrap();
        let asker = Handle::parse("bob").unwrap();
        let text = |fate| AskingText {
            asker: &asker,
            item: &mine,
            area: &area,
            holder_item: &theirs,
            fate,
        };

        let left = text(Asking::LeftToHolder).lead();
        for part in [
            "bob asks you for a grant: their work item 7 (\"Refund rounding\") needs edits in api:billing, which your claim on work item gh:acme/widgets#42 holds.",
            "dibs grant 7 --area api:billing --to bob",
            "A revoke of your grant to bob in api:billing stands while both claims last, so dibs leaves this request to you to grant or answer: it does not grant it on your behalf.",
        ] {
            assert!(left.contains(part), "{part}: {left}");
        }
        assert!(!left.contains("have passed"), "{left}");
        assert_eq!(Asking::LeftToHolder.importance(), Importance::High);

        let granted = text(Asking::GrantedAtOnce).lead();
        assert_eq!(
            granted,
            "bob asked you for a grant: their work item 7 (\"Refund rounding\") needs edits in api:billing, which your claim on work item gh:acme/widgets#42 holds.\n\
             dibs granted it at once on your behalf; its reply in this conversation says why and how to undo it."
        );
        assert_eq!(Asking::GrantedAtOnce.importance(), Importance::Normal);
    }
}
