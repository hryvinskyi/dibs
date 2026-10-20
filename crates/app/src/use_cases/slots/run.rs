//! The heartbeat of a run holding a slot, and another member taking over a hold whose run went
//! silent.

use std::sync::Arc;

use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{RunId, SessionId, ThreadId, WaiterId};
use dibs_domain::message::Body;
use dibs_domain::slot::{
    Liveness, Purpose, SlotName, TakeRequest, decide_run_beat, decide_stall_takeover,
};

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{Names, refuse};
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::use_cases::common::own_session;
use crate::use_cases::slots::end::declared;
use crate::use_cases::slots::scene::{Notice, Scene};
use crate::use_cases::slots::take::line_of;
use crate::use_cases::slots::view::{NamedHold, SlotLine};
use crate::work::Work;

/// A run of `dibs slot run` says it is alive: its hold is seen now, and a stalled hold is live
/// again. A hold the run no longer has is answered with who holds the slot now, if anyone, and
/// nothing changes.
pub struct SlotRunBeat {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
}

impl SlotRunBeat {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            notifier: deps.notifier.clone(),
            hot_rules: deps.hot_rules.clone(),
        }
    }

    /// Records the heartbeat.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::Slot`] for an undeclared slot;
    /// [`AppError::Beat`] when the run's hold is gone, naming who took it over;
    /// [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        slot: &SlotName,
        run: RunId,
    ) -> Result<SlotLine, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let mut work = Work::begin(&*self.store, actor.project).await?;
        let mut scene = Scene::load(&mut work, &self.hot_rules, now).await?;
        declared(&mut work, &scene, slot).await?;
        let beat = match decide_run_beat(&scene.board, slot, actor.member, run, now) {
            Ok(beat) => beat,
            Err(reason) => {
                scene.save(&mut work, now).await?;
                let refusal = refuse(&mut work, reason).await;
                work.commit(&*self.notifier).await?;
                return Err(refusal);
            }
        };
        scene.apply(beat);
        scene.save(&mut work, now).await?;
        let line = line_of(&mut work, &scene, slot).await?;
        work.commit(&*self.notifier).await?;
        Ok(line)
    }
}

/// The takeover to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakeOverSlotInput {
    /// The slot.
    pub slot: SlotName,
    /// The caller's session asking.
    pub session: SessionId,
    /// Why the caller holds it now, as every member reads it.
    pub purpose: Purpose,
    /// What the caller checked before taking it over, as told to the previous holder.
    pub reason: Body,
}

/// The takeover made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakenOverSlot {
    /// The previous hold.
    pub previous: NamedHold,
    /// The handle of the caller, who holds the slot now.
    pub holder: Handle,
    /// The thread that told the previous holder.
    pub thread: ThreadId,
    /// The slot and its group now.
    pub line: SlotLine,
}

/// Takes over another member's stalled hold — one whose run stopped sending heartbeats — and tells
/// that member the reason in a thread of its own. A live hold is never taken over.
pub struct TakeOverSlot {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
}

impl TakeOverSlot {
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

    /// Makes the takeover.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] for a session not the
    /// caller's; [`AppError::Slot`] for an undeclared slot, one nobody holds, the caller's own
    /// hold or one that is not stalled; [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        input: TakeOverSlotInput,
    ) -> Result<TakenOverSlot, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let mut work = Work::begin(&*self.store, actor.project).await?;
        let session = own_session(&mut work, actor, input.session).await?;
        let mut scene = Scene::load(&mut work, &self.hot_rules, now).await?;
        let request = TakeRequest {
            slot: input.slot.clone(),
            member: actor.member,
            session: session.id,
            host: session.host,
            purpose: input.purpose,
            item: None,
            liveness: Liveness::Session,
            waiter: WaiterId(self.ids.next()),
        };
        let transition = match decide_stall_takeover(&scene.board, scene.catalog(), &request, now) {
            Ok(transition) => transition,
            Err(reason) => return Err(refuse(&mut work, reason).await),
        };
        let previous = scene.apply(transition);
        scene.save(&mut work, now).await?;
        let thread = Notice {
            ids: &*self.ids,
            from: actor.member,
            label: session.label,
            to: previous.member,
            subject: format!("Your stalled hold on {} was taken over", input.slot),
            body: input.reason,
        }
        .send(&mut work, now)
        .await?;
        let names = Names::load(&mut work).await?;
        let previous = NamedHold {
            holder: names.handle(previous.member)?,
            hold: previous,
        };
        let holder = names.handle(actor.member)?;
        let line = line_of(&mut work, &scene, &input.slot).await?;
        work.commit(&*self.notifier).await?;
        Ok(TakenOverSlot {
            previous,
            holder,
            thread,
            line,
        })
    }
}
