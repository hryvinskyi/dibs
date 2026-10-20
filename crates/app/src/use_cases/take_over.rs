//! Taking over another member's stale claim.

use std::sync::Arc;

use dibs_domain::ref_scheme::RefSchemeRegistry;

use dibs_domain::claim::{ClaimRequest, Takeover, decide_takeover};
use dibs_domain::event::{EventKind, TakenOver};
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{ItemId, SessionId, ThreadId};
use dibs_domain::message::{Author, Body, Importance, Recipients};

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{ItemName, Names, refuse};
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::use_cases::claim_item::widened;
use crate::use_cases::common::{existing_item, own_session, remove_claim, resolve_areas};
use crate::use_cases::grant_endings::end_orphaned_grants;
use crate::use_cases::wide_asks::{Announced, Announcer};
use crate::use_cases::writing::{Posted, SystemPost, system_body};
use crate::work::Work;

const SUBJECT_PREFIX: &str = "Taken over: ";

/// The takeover to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakeOverInput {
    /// The item, by local id.
    pub item: ItemId,
    /// The caller's session asking.
    pub session: SessionId,
    /// The areas to declare, as a person names them.
    pub areas: Vec<String>,
    /// Why, as told to the previous holder.
    pub reason: Body,
}

/// The takeover made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakenOverClaim {
    /// The ended claim and the new one.
    pub takeover: Takeover,
    /// The thread that told the previous holder.
    pub thread: ThreadId,
    /// The handle of the new claim's member, the caller.
    pub holder: dibs_domain::handle::Handle,
    /// The handle of the previous claim's member.
    pub previous_holder: dibs_domain::handle::Handle,
    /// The item.
    pub item: ItemName,
    /// The announcements the new claim's wide scope sent to the holders of its areas: a wide scope
    /// carried on is announced again, in the new member's name.
    pub announced: Vec<Announced>,
}

/// Takes over another member's stale claim on an item. In the same transaction the previous claim
/// ends, with the paths recorded under it and the grants resting on it, and the previous holder is
/// sent the reason in a thread about the item, as a `high` message that asks for no
/// acknowledgement, so it reaches them while they work. A live claim is never taken over. A wide
/// scope the new claim carries on is announced to the holders of its areas in the new member's
/// name, since what they acknowledged before granted the previous member, and those grants end.
pub struct TakeOver {
    ref_schemes: Arc<RefSchemeRegistry>,
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
}

impl TakeOver {
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

    /// Makes the takeover.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] for an unknown item or a
    /// session not the caller's; [`AppError::ItemClosed`] for a done or dropped item;
    /// [`AppError::Area`] for an area that does not resolve; [`AppError::Takeover`] when there is
    /// nothing to take, the claim is the caller's or still live, or a declared area is held;
    /// [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        input: TakeOverInput,
    ) -> Result<TakenOverClaim, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let project = actor.project;
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let session = own_session(&mut work, actor, input.session).await?;
        let item = existing_item(&mut work, input.item).await?;
        if item.is_closed() {
            return Err(AppError::ItemClosed {
                status: item.status,
            });
        }
        let settings = work.project_settings(&self.hot_rules).await?;
        let request = ClaimRequest {
            item: item.id,
            by: actor.member,
            session: session.id,
            host: session.host.clone(),
            areas: resolve_areas(&settings, &input.areas)?,
            wide: None,
        };
        let current = work.claim(project, item.id).await?;
        let others = work.claims(project).await?;
        let takeover = match decide_takeover(current.as_ref(), &others, &request, now, &settings) {
            Ok(takeover) => takeover,
            Err(reason) => return Err(refuse(&mut work, reason).await),
        };
        remove_claim(&mut work, &takeover.previous).await?;
        work.save_claim(project, &takeover.claim).await?;
        work.record(
            now,
            EventKind::TakenOver(TakenOver {
                item: item.id,
                member: actor.member,
                previous: takeover.previous.member,
                session: session.id,
                host: session.host.clone(),
                areas: takeover.claim.areas.clone(),
            }),
        )
        .await?;
        let names = Names::load(&mut work).await?;
        let holder = names.handle(actor.member)?;
        let named = ItemName::of(&item, &self.ref_schemes);
        let thread = SystemPost {
            ids: Posted::next(&*self.ids),
            subject: format!("{SUBJECT_PREFIX}{}", item.title),
            item: Some(item.id),
            on_behalf_of: Author::Member(actor.member),
            to: Recipients::Members([takeover.previous.member].into()),
            body: system_body(
                &format!(
                    "{holder} took over your stale claim on work item {} \"{}\". Your claim has ended; the item is theirs now.",
                    named
                        .external_ref
                        .as_ref()
                        .map_or_else(|| format!("#{}", item.id), ToString::to_string),
                    item.title
                ),
                Some(("Their reason", input.reason.as_str())),
            )?,
            importance: Importance::High,
            ack_required: false,
        }
        .send(&mut work, now)
        .await?
        .thread;
        if let Some(widened) = widened(None, &takeover.claim) {
            work.record(now, widened).await?;
        }
        end_orphaned_grants(&mut work, now).await?;
        let announced = Announcer {
            ids: &*self.ids,
            schemes: &self.ref_schemes,
            settings: &settings,
            now,
        }
        .announce_due(&mut work)
        .await?
        .into_iter()
        .filter(|announced| announced.ask.from == actor.member)
        .collect();
        let previous_holder = names.handle(takeover.previous.member)?;
        work.commit(&*self.notifier).await?;
        Ok(TakenOverClaim {
            item: named,
            takeover,
            thread,
            holder,
            previous_holder,
            announced,
        })
    }
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use dibs_domain::area::QualifiedArea;
    use dibs_domain::claim::TakeoverRefused;
    use dibs_domain::event::{GrantEnd, GrantRevoked, ThreadStarted};
    use dibs_domain::handle::Handle;
    use dibs_domain::message::may_read;

    use super::*;
    use crate::named::Refusal;
    use crate::use_cases::testing::Env;
    use crate::use_cases::{ClaimItem, ClaimItemInput, Grant, GrantInput, Inbox};

    fn reason() -> Body {
        Body::parse("You have been away a day; I am finishing the invoice fix.").unwrap()
    }

    async fn bob_claims(env: &Env, item: ItemId, areas: &[&str]) {
        ClaimItem::new(&env.deps)
            .execute(
                &env.bob.actor,
                ClaimItemInput {
                    wide: None,
                    item,
                    session: env.bob.session,
                    areas: areas.iter().map(|a| (*a).to_owned()).collect(),
                },
            )
            .await
            .unwrap();
    }

    fn alice_takes(env: &Env, item: ItemId, areas: &[&str]) -> TakeOverInput {
        TakeOverInput {
            item,
            session: env.alice.session,
            areas: areas.iter().map(|a| (*a).to_owned()).collect(),
            reason: reason(),
        }
    }

    #[tokio::test]
    async fn a_stale_claim_is_taken_over_and_its_holder_told_why() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        bob_claims(&env, item, &["billing"]).await;
        env.advance(Duration::hours(25));
        let before = env.events().await.len();
        let taken = TakeOver::new(&env.deps)
            .execute(&env.alice.actor, alice_takes(&env, item, &["search"]))
            .await
            .unwrap();
        assert_eq!(taken.takeover.previous.member, env.bob.actor.member);
        assert_eq!(taken.takeover.claim.member, env.alice.actor.member);
        assert_eq!(
            taken.takeover.claim.areas,
            [
                QualifiedArea::parse("default:billing").unwrap(),
                QualifiedArea::parse("default:search").unwrap()
            ]
            .into()
        );
        let kinds = env.kinds_after(before).await;
        assert_eq!(kinds.len(), 3, "{kinds:?}");
        let EventKind::TakenOver(event) = &kinds[0] else {
            panic!("expected taken_over, got {kinds:?}");
        };
        assert_eq!(event.previous, env.bob.actor.member);
        assert_eq!(
            kinds[1],
            EventKind::ThreadStarted(ThreadStarted {
                thread: taken.thread,
                author: Some(env.alice.actor.member),
                item: Some(item),
            })
        );
        assert!(matches!(kinds[2], EventKind::MessagePosted(_)));
        let mut tx = env.deps.store.begin().await.unwrap();
        let thread = tx.thread(env.project, taken.thread).await.unwrap().unwrap();
        assert_eq!(
            thread.subject.as_str(),
            "Taken over: Work on gh:acme/widgets#1"
        );
        assert!(may_read(
            thread.author,
            &thread.recipients,
            env.bob.actor.member,
            env.bob.actor.role
        ));
        let messages = tx.messages(env.project, taken.thread).await.unwrap();
        assert_eq!(
            messages[0].author,
            Author::System,
            "dibs tells them, for alice"
        );
        assert_eq!(messages[0].label.as_str(), "dibs");
        let text = messages[0].body.as_str();
        assert!(
            text.starts_with("alice took over your stale claim on work item gh:acme/widgets#1"),
            "{text}"
        );
        assert!(
            text.ends_with(&format!("Their reason: {}", reason().as_str())),
            "{text}"
        );
        assert_eq!(
            (messages[0].importance, messages[0].ack_required),
            (Importance::High, false),
            "the previous holder hears it soon and is asked for nothing"
        );
        let stored = tx.claim(env.project, item).await.unwrap().unwrap();
        assert_eq!(stored, taken.takeover.claim);
        drop(tx);
        let inbox = Inbox::new(&env.deps)
            .execute(
                &env.bob.actor,
                crate::use_cases::InboxQuery {
                    unread_only: true,
                    unacked: false,
                    deliverable: false,
                },
            )
            .await
            .unwrap()
            .threads;
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].unread, 1);
    }

    #[tokio::test]
    async fn a_live_claim_is_never_taken_over() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        bob_claims(&env, item, &[]).await;
        let before = env.events().await.len();
        let error = TakeOver::new(&env.deps)
            .execute(&env.alice.actor, alice_takes(&env, item, &[]))
            .await
            .unwrap_err();
        assert_eq!(error.code(), "not_stale");
        assert!(matches!(
            &error,
            AppError::Takeover(Refusal {
                reason: TakeoverRefused::NotStale { .. },
                member: Some(handle),
                ..
            }) if handle.as_str() == "bob"
        ));
        assert_eq!(env.events().await.len(), before);
    }

    #[tokio::test]
    async fn nothing_to_take_and_own_claims_are_refused() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        let take = TakeOver::new(&env.deps);
        let nothing = take
            .execute(&env.alice.actor, alice_takes(&env, item, &[]))
            .await
            .unwrap_err();
        assert_eq!(nothing.code(), "nothing_to_take");
        bob_claims(&env, item, &[]).await;
        env.advance(Duration::hours(25));
        let own = take
            .execute(
                &env.bob.actor,
                TakeOverInput {
                    session: env.bob.session,
                    ..alice_takes(&env, item, &[])
                },
            )
            .await
            .unwrap_err();
        assert_eq!(own.code(), "own_claim");
        let integration = take
            .execute(&env.importer, alice_takes(&env, item, &[]))
            .await
            .unwrap_err();
        assert_eq!(integration.code(), "forbidden");
    }

    #[tokio::test]
    async fn the_previous_holders_grants_end_with_their_claim() {
        let env = Env::new().await;
        let bobs = env.item("gh:acme/widgets#1").await;
        let alices = env.item("gh:acme/widgets#2").await;
        bob_claims(&env, bobs, &["billing"]).await;
        ClaimItem::new(&env.deps)
            .execute(
                &env.alice.actor,
                ClaimItemInput {
                    wide: None,
                    item: alices,
                    session: env.alice.session,
                    areas: Vec::new(),
                },
            )
            .await
            .unwrap();
        let granted = Grant::new(&env.deps)
            .execute(
                &env.bob.actor,
                GrantInput {
                    item: alices,
                    area: "billing".to_owned(),
                    to: Handle::parse("alice").unwrap(),
                },
            )
            .await
            .unwrap();
        env.advance(Duration::hours(25));
        let before = env.events().await.len();
        TakeOver::new(&env.deps)
            .execute(&env.alice.actor, alice_takes(&env, bobs, &[]))
            .await
            .unwrap();
        let kinds = env.kinds_after(before).await;
        assert_eq!(
            kinds.last(),
            Some(&EventKind::GrantRevoked(GrantRevoked {
                grant: granted.grant.record.id,
                item: alices,
                area: QualifiedArea::parse("default:billing").unwrap(),
                from: env.bob.actor.member,
                to: env.alice.actor.member,
                cause: GrantEnd::GrantorLeft,
            }))
        );
    }
}
