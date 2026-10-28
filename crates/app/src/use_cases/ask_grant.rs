//! A member asking whoever holds an area for a grant, in a conversation that reaches the session
//! working under the holder's claim.

use std::sync::Arc;

use dibs_domain::area::QualifiedArea;
use dibs_domain::grant::{AskOutcome, AskedGrant, GrantAsk, decide_grant_ask};
use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{ItemId, ThreadId};
use dibs_domain::message::{Importance, Recipients};
use dibs_domain::ref_scheme::RefSchemeRegistry;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{ItemName, Names};
use crate::ports::{Clock, EventNotifier, IdGen, Store, StoreError};
use crate::use_cases::common::existing_item;
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
    },
    /// No live claim holds the area; the caller can claim it.
    Free,
    /// A live claim of the caller's own holds the area.
    Own,
    /// The holder's grant to the caller in the area is already in force.
    Granted {
        /// The holder who granted.
        holder: Handle,
        /// The item of the holder's claim that holds the area.
        holder_item: ItemName,
    },
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
/// it reaches the session working under the claim that holds the area; its first message is high,
/// asks for no acknowledgement, and says how to give the grant. The request is recorded with it, so
/// the holder's inbox lists it until it is decided.
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
    /// holds no live claim on the item; [`AppError::ImportanceLimited`] when the caller already
    /// wrote as many high or urgent messages within the hour as the project allows;
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
        let grants: Vec<_> = work
            .grants(project)
            .await?
            .into_iter()
            .map(|record| record.grant)
            .collect();
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
                let text = AskingText {
                    asker: &asker,
                    item: &item_name,
                    area: &ask.area,
                    holder_item: &holder_name,
                };
                let (thread, message) = MemberThread {
                    ids: Posted::next(&*self.ids),
                    subject: fit_subject(&text.subject())?,
                    item: Some(holder_item),
                    author: actor.member,
                    to: Recipients::members([holder].into())?,
                    label,
                    body: titled_body(&text.lead(), None)?,
                    importance: Importance::High,
                    ack_required: false,
                }
                .start(&mut work, settings.raised_per_hour(), now)
                .await?;
                let asked = AskedGrant {
                    message: message.id,
                    ask: ask.clone(),
                    holder,
                };
                work.insert_asked_grant(project, &asked).await?;
                GrantAsked::Asked {
                    holder: names.handle(holder)?,
                    holder_item: holder_name,
                    thread: thread.id,
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

    /// The first message: who asks, for which item, which claim holds the area, and how to give
    /// or refuse the grant.
    fn lead(&self) -> String {
        let asker = self.asker;
        let area = self.area;
        let key = self.item.key();
        let holder_item = self.holder_item.key();
        format!(
            "{asker} asks you for a grant: their work item {key} (\"{title}\") needs edits in {area}, which your claim on work item {holder_item} holds.\n\
             To give it: dibs grant {key} --area {area} --to {asker} (MCP: dibs_grant with key {key}, area {area}, to {asker}). It lasts while both claims do; you can revoke it. dibs tells {asker} when it is given.\n\
             To refuse it or ask a question, answer in this conversation. Nothing is granted until you give it.",
            title = self.item.title,
        )
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
        } = granted.outcome
        else {
            panic!("not granted: {granted:?}");
        };
        assert_eq!(holder.as_str(), "alice");
        assert_eq!(holder_item.id, alices);
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
        };
        assert_eq!(text.subject(), "A grant on api:billing for Refund rounding");
        let lead = text.lead();
        for part in [
            "bob asks you for a grant: their work item 7 (\"Refund rounding\") needs edits in api:billing",
            "your claim on work item gh:acme/widgets#42 holds",
            "dibs grant 7 --area api:billing --to bob",
            "MCP: dibs_grant with key 7, area api:billing, to bob",
        ] {
            assert!(lead.contains(part), "{part}: {lead}");
        }
    }
}
