//! Claiming the delivery of a pressing message or a slot offer to the caller's agent: the first of
//! the caller's sessions to claim a subject delivers it, and every later claim is refused, so a
//! member hears of it once however many sessions they run.

use std::sync::Arc;

use time::OffsetDateTime;

use dibs_domain::delivery::{Delivery, DeliveryRefused, DeliverySubject, decide_delivery};
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{MessageId, SessionId};
use dibs_domain::slot::SlotName;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::use_cases::ack::readable_message;
use crate::use_cases::acks::{people, reached};
use crate::use_cases::common::own_session;
use crate::use_cases::slots::Scene;
use crate::work::Work;

/// What the caller's session claims to deliver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryAsk {
    /// A message of a thread the caller may read, sent to them.
    Message(MessageId),
    /// The slot on offer to the caller until `until`.
    Offer {
        /// The slot.
        slot: SlotName,
        /// The offer's deadline, as the caller was told it.
        until: OffsetDateTime,
    },
}

/// Claims the delivery of a message or an offer for one session of the caller, in one
/// transaction. A message must reach the caller (they are one of its recipients, never its
/// author); an offer must stand, to the caller, with that deadline, after what time alone changed
/// about the slots is applied. Claiming records no event: it changes nothing anyone else sees.
pub struct ClaimDelivery {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
}

impl ClaimDelivery {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            ids: deps.ids.clone(),
            notifier: deps.notifier.clone(),
            hot_rules: deps.hot_rules.clone(),
        }
    }

    /// Claims the delivery for `session`.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] for a session that is not
    /// the caller's or a message they may not read; [`AppError::Delivery`] when the message does
    /// not reach them, the offer does not stand, or the subject was delivered to them already;
    /// [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        session: SessionId,
        ask: DeliveryAsk,
    ) -> Result<Delivery, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let project = actor.project;
        let mut work = Work::begin_for(&*self.store, actor).await?;
        own_session(&mut work, actor, session).await?;
        let subject = match ask {
            DeliveryAsk::Message(id) => {
                let (message, thread) = readable_message(&mut work, actor, id).await?;
                let people = people(&mut work).await?;
                if !reached(&thread, &message, &people).contains(&actor.member) {
                    return Err(DeliveryRefused::NotRecipient.into());
                }
                DeliverySubject::Message(id)
            }
            DeliveryAsk::Offer { slot, until } => {
                let mut scene = Scene::load(&mut work, &self.hot_rules, now).await?;
                scene.save(&mut work, &*self.ids, now).await?;
                let stands = scene.board.offers.iter().any(|offer| {
                    offer.member == actor.member && offer.slot == slot && offer.until == until
                });
                if !stands {
                    work.commit(&*self.notifier).await?;
                    return Err(DeliveryRefused::OfferGone.into());
                }
                DeliverySubject::Offer { slot, until }
            }
        };
        let existing = work.delivery(project, actor.member, &subject).await?;
        let delivery = decide_delivery(existing.as_ref(), actor.member, subject, session, now)?;
        work.insert_delivery(project, &delivery).await?;
        work.commit(&*self.notifier).await?;
        Ok(delivery)
    }
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use dibs_domain::handle::Handle;
    use dibs_domain::message::{Body, Importance, SessionLabel, Subject};
    use dibs_domain::slot::Purpose;

    use super::*;
    use crate::use_cases::testing::{Env, session};
    use crate::use_cases::{
        Addressees, HoldMode, ReleaseSlot, ReleaseSlotInput, StartThread, StartThreadInput,
        TakeSlot, TakeSlotInput,
    };

    async fn urgent_to_bob(env: &Env) -> MessageId {
        StartThread::new(&env.deps)
            .execute(
                &env.alice.actor,
                StartThreadInput {
                    subject: Subject::parse("Staging is down").unwrap(),
                    to: Addressees::Members(vec![Handle::parse("bob").unwrap()]),
                    item: None,
                    label: SessionLabel::parse("web").unwrap(),
                    body: Body::parse("Hold the deploy.").unwrap(),
                    importance: Importance::Urgent,
                    ack_required: false,
                },
            )
            .await
            .unwrap()
            .message
            .record
            .id
    }

    #[tokio::test]
    async fn the_first_session_to_claim_a_message_delivers_it_and_the_second_is_told() {
        let env = Env::new().await;
        let message = urgent_to_bob(&env).await;
        let claim = ClaimDelivery::new(&env.deps);
        let first = claim
            .execute(
                &env.bob.actor,
                env.bob.session,
                DeliveryAsk::Message(message),
            )
            .await
            .unwrap();
        assert_eq!(first.subject, DeliverySubject::Message(message));
        assert_eq!(first.session, env.bob.session);
        let other = session(&env.deps, &env.bob.actor, "laptop", "laptop/0b0b0b0b").await;
        env.advance(Duration::seconds(5));
        let events = env.events().await.len();
        for session in [other, env.bob.session] {
            let refused = claim
                .execute(&env.bob.actor, session, DeliveryAsk::Message(message))
                .await
                .unwrap_err();
            assert_eq!(refused.code(), "delivered");
        }
        assert_eq!(env.events().await.len(), events, "a claim records no event");
    }

    #[tokio::test]
    async fn a_message_is_delivered_only_to_whom_it_reaches_from_their_own_session() {
        let env = Env::new().await;
        let message = urgent_to_bob(&env).await;
        let claim = ClaimDelivery::new(&env.deps);
        let code = |actor: Actor, session: SessionId, id: MessageId| {
            let claim = &claim;
            async move {
                claim
                    .execute(&actor, session, DeliveryAsk::Message(id))
                    .await
                    .unwrap_err()
                    .code()
            }
        };
        assert_eq!(
            code(env.alice.actor.clone(), env.alice.session, message).await,
            "not_a_recipient",
            "never to its own author"
        );
        assert_eq!(
            code(env.bob.actor.clone(), env.alice.session, message).await,
            "not_found",
            "a session of another member"
        );
        assert_eq!(
            code(env.bob.actor.clone(), env.bob.session, MessageId(999_999)).await,
            "not_found"
        );
        assert_eq!(
            claim
                .execute(
                    &env.importer,
                    env.bob.session,
                    DeliveryAsk::Message(message)
                )
                .await
                .unwrap_err()
                .code(),
            "forbidden"
        );
    }

    #[tokio::test]
    async fn an_offer_is_claimed_while_it_stands_and_a_lapsed_one_is_gone() {
        let env = Env::with_settings(
            r#"{"slots": [{"name": "deploy:prod", "max_hold": "30m", "offer_for": "5m"}]}"#,
        )
        .await;
        let slot = SlotName::parse("deploy:prod").unwrap();
        let take = |actor: Actor, session: SessionId| {
            let env = &env;
            let slot = slot.clone();
            async move {
                TakeSlot::new(&env.deps)
                    .execute(
                        &actor,
                        TakeSlotInput {
                            slot,
                            session,
                            purpose: Purpose::parse("ship 2.4").unwrap(),
                            item: None,
                            mode: HoldMode::Session,
                        },
                    )
                    .await
                    .unwrap();
            }
        };
        take(env.alice.actor.clone(), env.alice.session).await;
        take(env.bob.actor.clone(), env.bob.session).await;
        ReleaseSlot::new(&env.deps)
            .execute(
                &env.alice.actor,
                ReleaseSlotInput {
                    slot: slot.clone(),
                    run: None,
                    exit: None,
                },
            )
            .await
            .unwrap();
        let until = env.now() + Duration::minutes(5);
        let claim = ClaimDelivery::new(&env.deps);
        let ask = |until: OffsetDateTime| DeliveryAsk::Offer {
            slot: slot.clone(),
            until,
        };
        assert_eq!(
            claim
                .execute(
                    &env.bob.actor,
                    env.bob.session,
                    ask(until + Duration::seconds(1))
                )
                .await
                .unwrap_err()
                .code(),
            "offer_gone",
            "another deadline is another offer"
        );
        assert_eq!(
            claim
                .execute(&env.alice.actor, env.alice.session, ask(until))
                .await
                .unwrap_err()
                .code(),
            "offer_gone",
            "an offer to someone else"
        );
        let won = claim
            .execute(&env.bob.actor, env.bob.session, ask(until))
            .await
            .unwrap();
        assert_eq!(
            won.subject.key(),
            format!("offer:deploy:prod:{}", until.unix_timestamp_nanos())
        );
        assert_eq!(
            claim
                .execute(&env.bob.actor, env.bob.session, ask(until))
                .await
                .unwrap_err()
                .code(),
            "delivered"
        );
        env.advance(Duration::minutes(6));
        assert_eq!(
            claim
                .execute(&env.bob.actor, env.bob.session, ask(until))
                .await
                .unwrap_err()
                .code(),
            "offer_gone",
            "a lapsed offer"
        );
    }
}
