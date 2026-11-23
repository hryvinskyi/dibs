//! A holder letting another member edit inside an area they hold.

use std::sync::Arc;

use time::OffsetDateTime;

use dibs_domain::area::QualifiedArea;
use dibs_domain::claim::Claim;
use dibs_domain::event::{EventKind, Granted};
use dibs_domain::grant::{self, GrantAsk, GrantDecision, GrantOutcome, GrantRefused, decide_grant};
use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{GrantId, ItemId};
use dibs_domain::message::{Author, Importance, Recipients};
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_domain::settings::ProjectSettings;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{ItemName, NamedGrant, Names};
use crate::ports::{Clock, EventNotifier, IdGen, Store, StoreError};
use crate::records::GrantRecord;
use crate::use_cases::claim_notice::ClaimNotice;
use crate::use_cases::common::{existing_item, member_by_handle};
use crate::use_cases::writing::{Posted, SystemPost, titled_body};
use crate::work::Work;

/// The grant to give.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantInput {
    /// The grantee's work item: the work that needs the edit.
    pub item: ItemId,
    /// The area, as a person names it.
    pub area: String,
    /// The grantee.
    pub to: Handle,
}

/// The grant in force.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantResult {
    /// The grant, with the handles of its two members.
    pub grant: NamedGrant,
    /// Whether this call gave it, rather than finding it given.
    pub created: bool,
    /// The grantee's item.
    pub item: ItemName,
}

/// Records the caller's consent that another member may edit inside an area the caller holds, for
/// the grantee's live claim on an item, and tells the grantee in that claim's conversation, so
/// that it reaches the session working under their claim whether they asked for it or not. Should
/// the grantee hold no claim on the item, they are told in a conversation of its own about it,
/// started in the granter's name. Either message names the granter and how to write to them.
/// The grantee's requests to the caller for that area and item are settled with it, so dibs never
/// decides them on the caller's behalf. A request of theirs for another item lapses while the
/// grant is in force; once the grant ends, it is judged again with its wait timed from when it was
/// asked: dibs grants it at the next sweep if `grant_after` has passed since then, and once it has
/// otherwise, unless it closes or a revoke leaves it to the caller. Giving a grant that exists
/// changes nothing and tells nobody.
pub struct Grant {
    ref_schemes: Arc<RefSchemeRegistry>,
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
}

impl Grant {
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

    /// Gives the grant.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] for an unknown item or
    /// grantee; [`AppError::Area`] for an area that does not resolve; [`AppError::Grant`] when the
    /// caller does not hold the area, grants to themselves, or the grantee holds no live claim on
    /// the item; [`AppError::Store`] when the store fails.
    pub async fn execute(&self, actor: &Actor, input: GrantInput) -> Result<GrantResult, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let project = actor.project;
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let item = existing_item(&mut work, input.item).await?;
        let grantee = member_by_handle(&mut work, &input.to).await?;
        let settings = work.project_settings(&self.hot_rules).await?;
        let request = grant::Grant {
            item: item.id,
            area: settings.resolve_area(&input.area)?,
            from: actor.member,
            to: grantee.id,
        };
        let records = work.grants(project).await?;
        let claims = work.claims(project).await?;
        let item = ItemName::of(&item, &self.ref_schemes);
        let giving = GiveGrant {
            request,
            decided: GrantDecision::Holder,
            item: &item,
            records: &records,
            claims: &claims,
            settings: &settings,
            ids: &*self.ids,
            now,
        };
        match giving.give(&mut work).await? {
            Giving::Refused(refused) => Err(refused.into()),
            Giving::Existing(record) => Ok(GrantResult {
                grant: Names::load(&mut work).await?.grant(record)?,
                created: false,
                item,
            }),
            Giving::Given(grant) => {
                work.commit(&*self.notifier).await?;
                Ok(GrantResult {
                    grant,
                    created: true,
                    item,
                })
            }
        }
    }
}

/// What giving a grant came to.
#[derive(Debug)]
pub(crate) enum Giving {
    /// The grant is given now, and the grantee told.
    Given(NamedGrant),
    /// The same grant exists; nothing changed.
    Existing(GrantRecord),
    /// The rules of every grant refuse it; nothing changed.
    Refused(GrantRefused),
}

/// A grant from a holder to a grantee, and who decided it: the holder, or dibs on the holder's
/// behalf. Whoever decides, the grant is taken only as the rules of every grant allow it (see
/// [`decide_grant`]): the holder holds the area through a live claim, the grantee holds a live
/// claim on the item, and the same grant does not exist.
pub(crate) struct GiveGrant<'a> {
    /// The grant: `from` is the holder, `to` the grantee, `item` the grantee's item.
    pub(crate) request: grant::Grant,
    /// Who decided it.
    pub(crate) decided: GrantDecision,
    /// The grantee's item, `request.item`, as the grantee's notice names it.
    pub(crate) item: &'a ItemName,
    /// The project's grants as the caller's transaction sees them.
    pub(crate) records: &'a [GrantRecord],
    /// The project's claims as the caller's transaction sees them.
    pub(crate) claims: &'a [Claim],
    /// The project's settings.
    pub(crate) settings: &'a ProjectSettings,
    /// Where new ids come from.
    pub(crate) ids: &'a dyn IdGen,
    /// When.
    pub(crate) now: OffsetDateTime,
}

impl GiveGrant<'_> {
    /// Gives the grant inside the caller's transaction: stores it with who decided, records
    /// `granted`, tells the grantee, and settles every request the grantee put to the holder in
    /// the area for the grant's item that is not settled yet, so that dibs never decides a request
    /// the grant answered, even once the grant is revoked. A request for another of the grantee's
    /// items is not answered by it: it lapses while the grant is in force. Once the grant ends, it
    /// is judged again with its wait timed from when it was asked: dibs grants it at the next sweep
    /// if `grant_after` has passed since then, and once it has otherwise, unless it closes or a
    /// revoke leaves it to the holder. Changes nothing when the grant exists or is refused.
    pub(crate) async fn give(self, work: &mut Work) -> Result<Giving, AppError> {
        let project = work.project_id();
        let existing: Vec<grant::Grant> = self.records.iter().map(|r| r.grant.clone()).collect();
        let grant = match decide_grant(
            &self.request,
            &existing,
            self.claims,
            self.now,
            self.settings,
        ) {
            Err(refused) => return Ok(Giving::Refused(refused)),
            Ok(GrantOutcome::AlreadyGranted) => {
                let record = self
                    .records
                    .iter()
                    .find(|record| record.grant == self.request)
                    .cloned()
                    .ok_or_else(|| StoreError::Corrupt {
                        what: "a grant the domain found but the store did not return".to_owned(),
                    })?;
                return Ok(Giving::Existing(record));
            }
            Ok(GrantOutcome::New(grant)) => grant,
        };
        let record = GrantRecord {
            id: GrantId(self.ids.next()),
            grant,
            created_at: self.now,
            decided: self.decided,
        };
        work.insert_grant(project, &record).await?;
        work.record(
            self.now,
            EventKind::Granted(Granted {
                grant: record.id,
                item: record.grant.item,
                area: record.grant.area.clone(),
                from: record.grant.from,
                to: record.grant.to,
                decided: self.decided,
            }),
        )
        .await?;
        let grant = Names::load(work).await?.grant(record)?;
        let text = GrantedText {
            granter: &grant.from,
            item: self.item,
            area: &grant.record.grant.area,
            decided: self.decided,
        };
        let body = titled_body(&text.lead(), None)?;
        let grantee = grant.record.grant.to;
        match self
            .claims
            .iter()
            .find(|claim| claim.item == self.item.id && claim.member == grantee)
        {
            Some(claim) => {
                ClaimNotice {
                    claim,
                    item: self.item,
                    body,
                    importance: Importance::High,
                }
                .send(work, self.ids, self.now)
                .await?;
            }
            None => {
                SystemPost {
                    ids: Posted::next(self.ids),
                    subject: text.subject(),
                    item: Some(self.item.id),
                    on_behalf_of: Author::Member(grant.record.grant.from),
                    to: Recipients::members([grantee].into())?,
                    body,
                    importance: Importance::High,
                    ack_required: false,
                }
                .send(work, self.now)
                .await?;
            }
        }
        work.settle_asked_grants_for_item(
            project,
            &GrantAsk {
                asker: grantee,
                item: grant.record.grant.item,
                area: grant.record.grant.area.clone(),
            },
            grant.record.grant.from,
            self.now,
        )
        .await?;
        Ok(Giving::Given(grant))
    }
}

/// What the grantee is told when a grant is given.
struct GrantedText<'a> {
    /// The holder who granted.
    granter: &'a Handle,
    /// The grantee's item, the grant's item.
    item: &'a ItemName,
    /// The area.
    area: &'a QualifiedArea,
    /// Who decided it: the holder, or dibs on their behalf.
    decided: GrantDecision,
}

impl GrantedText<'_> {
    /// The subject of a conversation of its own: the area, and the grantee's item by its reference,
    /// else its title.
    fn subject(&self) -> String {
        let item = self
            .item
            .external_ref
            .as_ref()
            .map_or_else(|| self.item.title.to_string(), ToString::to_string);
        format!("Granted: {} for {item}", self.area)
    }

    /// The message: who granted what, for which item — dibs on the holder's behalf, and why, when
    /// dibs decided — that the edit may go ahead now, how long the grant lasts, and how to write to
    /// the holder.
    fn lead(&self) -> String {
        if let Some(why) = self.decided.why() {
            return format!(
                "dibs granted you {area} for your work item {key} (\"{title}\") on {holder}'s behalf, because {why}. You may edit there now; dibs_check says granted. It lasts while both claims do, unless {holder} or an admin revokes it. To write to {holder}: dibs send {holder}.",
                holder = self.granter,
                area = self.area,
                key = self.item.key(),
                title = self.item.title,
            );
        }
        format!(
            "{granter} granted you {area} for your work item {key} (\"{title}\"). You may edit there now; dibs_check says granted. It lasts while both claims do, unless {granter} revokes it. To write to {granter}: dibs send {granter}.",
            granter = self.granter,
            area = self.area,
            key = self.item.key(),
            title = self.item.title,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::use_cases::testing::{Env, Person};
    use crate::use_cases::{ClaimItem, ClaimItemInput, DeliverableMessage, Inbox, InboxQuery};

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

    fn to_bob(item: ItemId, area: &str) -> GrantInput {
        GrantInput {
            item,
            area: area.to_owned(),
            to: Handle::parse("bob").unwrap(),
        }
    }

    #[tokio::test]
    async fn a_holder_grants_once_and_a_repeat_changes_nothing() {
        let env = Env::new().await;
        let alices = env.item("gh:acme/widgets#1").await;
        let bobs = env.item("gh:acme/widgets#2").await;
        claim(&env, &env.alice, alices, &["billing"]).await;
        claim(&env, &env.bob, bobs, &[]).await;
        let grant = Grant::new(&env.deps);
        let before = env.events().await.len();
        let given = grant
            .execute(&env.alice.actor, to_bob(bobs, "billing"))
            .await
            .unwrap();
        assert!(given.created);
        let kinds = env.kinds_after(before).await;
        assert_eq!(
            kinds[0],
            EventKind::Granted(Granted {
                grant: given.grant.record.id,
                item: bobs,
                area: QualifiedArea::parse("default:billing").unwrap(),
                from: env.alice.actor.member,
                to: env.bob.actor.member,
                decided: GrantDecision::Holder,
            })
        );
        assert!(
            matches!(
                &kinds[1..],
                [EventKind::ThreadStarted(started), EventKind::MessagePosted(_)]
                    if started.item == Some(bobs) && started.author.is_none()
            ),
            "dibs starts the grantee's claim conversation: {kinds:?}"
        );
        let after = env.events().await.len();
        let again = grant
            .execute(&env.alice.actor, to_bob(bobs, "default:billing"))
            .await
            .unwrap();
        assert!(!again.created);
        assert_eq!(again.grant, given.grant);
        assert_eq!(env.events().await.len(), after);
    }

    async fn deliverable(env: &Env, actor: &crate::actor::Actor) -> Vec<DeliverableMessage> {
        Inbox::new(&env.deps)
            .execute(
                actor,
                InboxQuery {
                    unread_only: false,
                    unacked: false,
                    deliverable: true,
                },
            )
            .await
            .unwrap()
            .deliverable
            .unwrap()
    }

    #[tokio::test]
    async fn the_grantee_is_told_between_their_tool_calls_and_the_granter_is_not() {
        let env = Env::new().await;
        let alices = env.item("gh:acme/widgets#1").await;
        let bobs = env.item("gh:acme/widgets#2").await;
        claim(&env, &env.alice, alices, &["billing"]).await;
        claim(&env, &env.bob, bobs, &[]).await;
        Grant::new(&env.deps)
            .execute(&env.alice.actor, to_bob(bobs, "billing"))
            .await
            .unwrap();
        let told = deliverable(&env, &env.bob.actor).await;
        assert_eq!(told.len(), 1, "{told:?}");
        assert_eq!(told[0].thread.record.item, Some(bobs));
        assert_eq!(
            told[0].thread.record.subject.as_str(),
            "Your claim on gh:acme/widgets#2 \"Work on gh:acme/widgets#2\"",
            "the grantee's claim conversation"
        );
        let body = told[0].message.record.body.as_str();
        assert!(
            body.starts_with(
                "alice granted you default:billing for your work item gh:acme/widgets#2"
            ),
            "{body}"
        );
        assert!(body.contains("You may edit there now"), "{body}");
        assert!(
            body.contains("To write to alice: dibs send alice."),
            "{body}"
        );
        assert!(deliverable(&env, &env.alice.actor).await.is_empty());
    }

    #[tokio::test]
    async fn every_rule_of_a_grant_is_enforced() {
        let env = Env::new().await;
        let alices = env.item("gh:acme/widgets#1").await;
        let bobs = env.item("gh:acme/widgets#2").await;
        let nobodys = env.item("gh:acme/widgets#3").await;
        claim(&env, &env.alice, alices, &["billing"]).await;
        claim(&env, &env.bob, bobs, &[]).await;
        let grant = Grant::new(&env.deps);
        let to_alice = GrantInput {
            to: Handle::parse("alice").unwrap(),
            ..to_bob(alices, "billing")
        };
        let cases = [
            (env.alice.actor.clone(), to_alice, "self_grant"),
            (
                env.alice.actor.clone(),
                to_bob(bobs, "search"),
                "not_holder",
            ),
            (
                env.alice.actor.clone(),
                to_bob(nobodys, "billing"),
                "grantee_has_no_claim",
            ),
            (
                env.alice.actor.clone(),
                to_bob(bobs, "nowhere"),
                "unknown_area",
            ),
            (
                env.alice.actor.clone(),
                to_bob(ItemId(9999), "billing"),
                "not_found",
            ),
            (
                env.alice.actor.clone(),
                GrantInput {
                    to: Handle::parse("nobody").unwrap(),
                    ..to_bob(bobs, "billing")
                },
                "not_found",
            ),
            (env.importer.clone(), to_bob(bobs, "billing"), "forbidden"),
        ];
        let before = env.events().await.len();
        for (actor, input, code) in cases {
            let error = grant.execute(&actor, input).await.unwrap_err();
            assert_eq!(error.code(), code);
        }
        assert_eq!(env.events().await.len(), before);
    }
}
