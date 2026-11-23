//! Taking over another member's stale claim.

use std::sync::Arc;

use dibs_domain::ref_scheme::RefSchemeRegistry;

use dibs_domain::claim::{ClaimRequest, Takeover, decide_takeover};
use dibs_domain::event::{EventKind, TakenOver};
use dibs_domain::handle::Handle;
use dibs_domain::handover::{AreaMove, ClaimKind, WithdrawReason, Withdrawal};
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{ItemId, SessionId, ThreadId};
use dibs_domain::message::{Body, Importance};
use dibs_domain::settings::format_duration;
use time::{Duration, OffsetDateTime};

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{ItemName, Names, refuse};
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::use_cases::claim_item::widened;
use crate::use_cases::claim_notice::ClaimNotice;
use crate::use_cases::common::{existing_item, own_session, remove_claim, resolve_areas};
use crate::use_cases::grant_endings::end_orphaned_grants;
use crate::use_cases::handover::{Handovers, save_taken_over};
use crate::use_cases::wide_asks::{Announced, Announcer};
use crate::use_cases::writing::titled_body;
use crate::work::Work;

/// The takeover to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakeOverInput {
    /// The item, by local id.
    pub item: ItemId,
    /// The caller's session asking.
    pub session: SessionId,
    /// The areas to declare, as a person names them.
    pub areas: Vec<String>,
    /// Why, as told to the previous holder; `None` has dibs write the standing reason, which says
    /// how long the claim showed no sign of work.
    pub reason: Option<Body>,
    /// The kind of the new claim; `None` carries on the previous claim's kind.
    pub kind: Option<ClaimKind>,
}

/// The takeover made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakenOverClaim {
    /// The ended claim and the new one.
    pub takeover: Takeover,
    /// The conversation of the previous claim, in which its holder was told.
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
/// told, with the reason, in the previous claim's conversation, as a `high` message that asks for
/// no acknowledgement, so it reaches them while they work; the message names the new holder and how
/// to write to them, since that conversation reaches the previous holder alone. Without a reason
/// from the caller, dibs writes the standing one: how long the claim showed no sign of work, and
/// that the new holder needs the item. A live claim is
/// never taken over. A wide scope the new claim carries on is announced to the holders of its
/// areas in the new member's name, since what they acknowledged before granted the previous
/// member, and those grants end.
/// An area another claim waited for the previous claim to hand over is not carried on: it moves
/// to the claim waiting for it. The areas a previous feature claim waited for carry on with it,
/// unless the new claim is debt work, which waits for nothing: those waits end.
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
            kind: input.kind,
        };
        let current = work.claim(project, item.id).await?;
        let others = work.claims(project).await?;
        let takeover = match decide_takeover(current.as_ref(), &others, &request, now, &settings) {
            Ok(takeover) => takeover,
            Err(reason) => return Err(refuse(&mut work, reason).await),
        };
        let ended: Vec<Withdrawal> = takeover
            .previous
            .pending
            .iter()
            .filter(|(area, _)| !takeover.claim.waits_for(area))
            .map(|(area, pending)| Withdrawal {
                moved: AreaMove {
                    area: area.clone(),
                    from: pending.from,
                    to: item.id,
                },
                reason: WithdrawReason::Ended,
                request: pending.request,
            })
            .collect();
        let handovers = Handovers {
            ids: &*self.ids,
            schemes: &self.ref_schemes,
            settings: &settings,
            now,
        };
        if !ended.is_empty() {
            let names = Names::load(&mut work).await?;
            for withdrawal in &ended {
                handovers
                    .withdraw(&mut work, &names, &others, withdrawal)
                    .await?;
            }
        }
        remove_claim(&mut work, &takeover.previous).await?;
        save_taken_over(&mut work, &takeover.claim).await?;
        work.record(
            now,
            EventKind::TakenOver(TakenOver {
                item: item.id,
                member: actor.member,
                previous: takeover.previous.member,
                session: session.id,
                host: session.host.clone(),
                areas: takeover.claim.areas.iter().cloned().collect(),
            }),
        )
        .await?;
        let names = Names::load(&mut work).await?;
        let holder = names.handle(actor.member)?;
        let named = ItemName::of(&item, &self.ref_schemes);
        let standing = match &input.reason {
            Some(_) => String::new(),
            None => format!(
                " {}",
                standing_reason(&holder, takeover.previous.last_seen, now)
            ),
        };
        let thread = ClaimNotice {
            claim: &takeover.previous,
            item: &named,
            body: titled_body(
                &format!(
                    "{holder} took over your stale claim on work item {} \"{}\". Your claim has ended; the item is theirs now.{standing} To write to {holder}: dibs send {holder}.",
                    named.label(),
                    item.title
                ),
                input
                    .reason
                    .as_ref()
                    .map(|reason| ("Their reason", reason.as_str())),
            )?,
            importance: Importance::High,
        }
        .send(&mut work, &*self.ids, now)
        .await?;
        if let Some(widened) = widened(None, &takeover.claim) {
            work.record(now, widened).await?;
        }
        end_orphaned_grants(&mut work, now).await?;
        handovers.settle(&mut work).await?;
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

/// The reason dibs gives the previous holder when the member taking over gave none: how long
/// nothing of the claim's work showed, from its last sign of work to `now` in whole minutes, and
/// that the taker needs the item.
fn standing_reason(taker: &Handle, last_seen: OffsetDateTime, now: OffsetDateTime) -> String {
    let silent = Duration::minutes((now - last_seen).whole_minutes());
    format!(
        "{taker} gave no reason. Nothing of your claim's work showed for {silent}. {taker} needs the work item.",
        silent = format_duration(silent),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use time::Duration;

    use dibs_domain::area::QualifiedArea;
    use dibs_domain::claim::TakeoverRefused;
    use dibs_domain::event::{GrantEnd, GrantRevoked, ThreadStarted};
    use dibs_domain::handle::Handle;
    use dibs_domain::message::{Author, Recipients, may_read};

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
                    branches: Default::default(),
                    wide: None,
                    item,
                    session: env.bob.session,
                    areas: areas.iter().map(|a| (*a).to_owned()).collect(),
                    kind: None,
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
            reason: Some(reason()),
            kind: None,
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
            BTreeSet::from([
                QualifiedArea::parse("default:billing").unwrap(),
                QualifiedArea::parse("default:search").unwrap()
            ])
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
                author: None,
                item: Some(item),
            }),
            "dibs starts the previous claim's conversation in nobody's name"
        );
        assert!(matches!(kinds[2], EventKind::MessagePosted(_)));
        let mut tx = env.deps.store.begin().await.unwrap();
        let thread = tx.thread(env.project, taken.thread).await.unwrap().unwrap();
        assert_eq!(
            thread.subject.as_str(),
            "Your claim on gh:acme/widgets#1 \"Work on gh:acme/widgets#1\""
        );
        assert_eq!(
            thread.recipients,
            Recipients::Members([env.bob.actor.member].into()),
            "the previous holder alone"
        );
        assert!(may_read(
            thread.author,
            &thread.recipients,
            env.bob.actor.member,
            env.bob.actor.role
        ));
        let messages = tx.messages(env.project, taken.thread).await.unwrap();
        assert_eq!(messages[0].author, Author::System, "dibs tells them");
        assert_eq!(messages[0].label.as_str(), "dibs");
        let text = messages[0].body.as_str();
        assert!(
            text.starts_with("alice took over your stale claim on work item gh:acme/widgets#1"),
            "{text}"
        );
        assert!(
            text.contains("To write to alice: dibs send alice."),
            "the taker is named with how to write to them: {text}"
        );
        assert!(
            text.ends_with(&format!("Their reason: {}", reason().as_str())),
            "{text}"
        );
        assert!(
            text.contains(crate::use_cases::writing::TITLE_NOTE),
            "a title is quoted as a member's or a tracker's words: {text}"
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
    async fn without_a_reason_dibs_tells_the_previous_holder_how_long_the_claim_was_silent() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        bob_claims(&env, item, &["billing"]).await;
        env.advance(Duration::hours(25) + Duration::seconds(42));
        let taken = TakeOver::new(&env.deps)
            .execute(
                &env.alice.actor,
                TakeOverInput {
                    reason: None,
                    ..alice_takes(&env, item, &[])
                },
            )
            .await
            .unwrap();
        assert_eq!(taken.takeover.claim.member, env.alice.actor.member);
        let mut tx = env.deps.store.begin().await.unwrap();
        let messages = tx.messages(env.project, taken.thread).await.unwrap();
        assert_eq!(messages.len(), 1);
        let text = messages[0].body.as_str();
        assert!(
            text.contains(
                "Your claim has ended; the item is theirs now. alice gave no reason. Nothing of your claim's work showed for 1d1h. alice needs the work item. To write to alice: dibs send alice."
            ),
            "the standing reason names the silent time in whole minutes and the taker: {text}"
        );
        assert!(!text.contains("Their reason"), "{text}");
        assert!(
            text.ends_with(crate::use_cases::writing::TITLE_NOTE),
            "{text}"
        );
    }

    #[test]
    fn the_standing_reason_counts_from_the_last_sign_of_work_in_whole_minutes() {
        let seen = crate::use_cases::testing::START;
        let alice = Handle::parse("alice").unwrap();
        assert_eq!(
            standing_reason(
                &alice,
                seen,
                seen + Duration::minutes(90) + Duration::seconds(59)
            ),
            "alice gave no reason. Nothing of your claim's work showed for 1h30m. alice needs the work item."
        );
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
                    branches: Default::default(),
                    wide: None,
                    item: alices,
                    session: env.alice.session,
                    areas: Vec::new(),
                    kind: None,
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
                by: None,
            }))
        );
    }
}
