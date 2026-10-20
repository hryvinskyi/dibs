//! Ending a hold or a place: the holder's release, a waiter leaving, an admin's forced release, and
//! the forge's report that a workflow run a hold waited for finished.

use std::sync::Arc;

use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{RunId, SessionId, ThreadId};
use dibs_domain::message::Body;
use dibs_domain::slot::{
    CommitSha, EndWord, RunExit, SlotName, SlotRefused, WorkflowName, decide_force_release,
    decide_leave, decide_release, decide_run_finished,
};

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{Names, refuse};
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::use_cases::common::own_session;
use crate::use_cases::slots::scene::{Notice, Scene};
use crate::use_cases::slots::take::line_of;
use crate::use_cases::slots::view::{NamedHold, SlotLine};
use crate::work::Work;

/// The ports every use case of this module uses.
struct Ports {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
}

impl Ports {
    fn of(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            notifier: deps.notifier.clone(),
            hot_rules: deps.hot_rules.clone(),
        }
    }
}

/// A hold that ended, named, and where its slot stands after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndedHold {
    /// The hold as it was.
    pub hold: NamedHold,
    /// The slot and its group now: offered to the first waiter, or free.
    pub line: SlotLine,
}

/// The release to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseSlotInput {
    /// The slot.
    pub slot: SlotName,
    /// The run releasing it, when a run holds it: then only that run's hold is released.
    pub run: Option<RunId>,
    /// How the run's command ended.
    pub exit: Option<RunExit>,
}

/// The holder releases a slot; its group's first waiter is offered it.
pub struct ReleaseSlot {
    ports: Ports,
}

impl ReleaseSlot {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            ports: Ports::of(deps),
        }
    }

    /// Releases the hold.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::Slot`] for an undeclared slot, one
    /// nobody or another member holds, or a run that no longer holds it; [`AppError::Store`] when
    /// the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        input: ReleaseSlotInput,
    ) -> Result<EndedHold, AppError> {
        actor.require_person()?;
        let now = self.ports.clock.now();
        let mut work = Work::begin(&*self.ports.store, actor.project).await?;
        let mut scene = Scene::load(&mut work, &self.ports.hot_rules, now).await?;
        declared(&mut work, &scene, &input.slot).await?;
        let decided = decide_release(
            &scene.board,
            scene.catalog(),
            &input.slot,
            actor.member,
            input.run,
            input.exit,
            now,
        );
        let transition = match decided {
            Ok(transition) => transition,
            Err(reason) => return Err(refuse(&mut work, reason).await),
        };
        let hold = scene.apply(transition);
        scene.save(&mut work, now).await?;
        let ended = ended(&mut work, &scene, hold, &input.slot).await?;
        work.commit(&*self.ports.notifier).await?;
        Ok(ended)
    }
}

/// A waiter leaves a slot group's queue; an offered member passes the offer to the next waiter.
pub struct LeaveSlotQueue {
    ports: Ports,
}

impl LeaveSlotQueue {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            ports: Ports::of(deps),
        }
    }

    /// Leaves the queue.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::Slot`] for an undeclared slot or a
    /// caller who is not waiting; [`AppError::Store`] when the store fails.
    pub async fn execute(&self, actor: &Actor, slot: &SlotName) -> Result<SlotLine, AppError> {
        actor.require_person()?;
        let now = self.ports.clock.now();
        let mut work = Work::begin(&*self.ports.store, actor.project).await?;
        let mut scene = Scene::load(&mut work, &self.ports.hot_rules, now).await?;
        let transition = match decide_leave(&scene.board, scene.catalog(), slot, actor.member, now)
        {
            Ok(transition) => transition,
            Err(reason) => return Err(refuse(&mut work, reason).await),
        };
        scene.apply(transition);
        scene.save(&mut work, now).await?;
        let line = line_of(&mut work, &scene, slot).await?;
        work.commit(&*self.ports.notifier).await?;
        Ok(line)
    }
}

/// The forced release to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForceReleaseSlotInput {
    /// The slot.
    pub slot: SlotName,
    /// The admin's session asking.
    pub session: SessionId,
    /// Why, as told to the holder.
    pub reason: Body,
}

/// A forced release, and the thread that told the holder why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForcedHold {
    /// The hold that ended and where the slot stands now.
    pub ended: EndedHold,
    /// The thread that told the holder; `None` when the admin held the slot.
    pub thread: Option<ThreadId>,
}

/// An admin ends any member's hold; the holder is sent the reason, and the group's first waiter
/// is offered the slot.
pub struct ForceReleaseSlot {
    ports: Ports,
    ids: Arc<dyn IdGen>,
}

impl ForceReleaseSlot {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            ports: Ports::of(deps),
            ids: deps.ids.clone(),
        }
    }

    /// Ends the hold. A caller who is no admin is refused before anything is looked up.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] when the caller is no admin; [`AppError::NotFound`] for a session
    /// not theirs; [`AppError::Slot`] for an undeclared slot or one nobody holds;
    /// [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        input: ForceReleaseSlotInput,
    ) -> Result<ForcedHold, AppError> {
        actor.require_admin()?;
        let now = self.ports.clock.now();
        let mut work = Work::begin(&*self.ports.store, actor.project).await?;
        let session = own_session(&mut work, actor, input.session).await?;
        let mut scene = Scene::load(&mut work, &self.ports.hot_rules, now).await?;
        declared(&mut work, &scene, &input.slot).await?;
        let decided = decide_force_release(
            &scene.board,
            scene.catalog(),
            &input.slot,
            actor.member,
            now,
        );
        let transition = match decided {
            Ok(transition) => transition,
            Err(reason) => return Err(refuse(&mut work, reason).await),
        };
        let hold = scene.apply(transition);
        scene.save(&mut work, now).await?;
        let thread = if hold.member == actor.member {
            None
        } else {
            let notice = Notice {
                ids: &*self.ids,
                from: actor.member,
                label: session.label,
                to: hold.member,
                subject: format!("Your hold on {} was ended by an admin", input.slot),
                body: input.reason,
            };
            Some(notice.send(&mut work, now).await?)
        };
        let ended = ended(&mut work, &scene, hold, &input.slot).await?;
        work.commit(&*self.ports.notifier).await?;
        Ok(ForcedHold { ended, thread })
    }
}

/// The forge's report that a workflow run finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunFinishedInput {
    /// The slot whose hold waited for the run.
    pub slot: SlotName,
    /// The workflow.
    pub workflow: WorkflowName,
    /// The commit it ran for.
    pub sha: CommitSha,
    /// How it concluded (`success`, `failure`, …).
    pub conclusion: EndWord,
}

/// Ends the hold tied to exactly this workflow and commit, if one is; a report that matches no hold
/// — a repeat, a slot since released or removed — changes nothing and is answered the same way, so
/// an integration may send it again safely.
pub struct SlotRunFinished {
    ports: Ports,
}

impl SlotRunFinished {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            ports: Ports::of(deps),
        }
    }

    /// Applies the report; answers whether it ended a hold.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for a member who is neither the importer nor an admin;
    /// [`AppError::Store`] when the store fails.
    pub async fn execute(&self, actor: &Actor, input: RunFinishedInput) -> Result<bool, AppError> {
        actor.require_import()?;
        let now = self.ports.clock.now();
        let mut work = Work::begin(&*self.ports.store, actor.project).await?;
        let mut scene = Scene::load(&mut work, &self.ports.hot_rules, now).await?;
        let transition = decide_run_finished(
            &scene.board,
            scene.catalog(),
            &input.slot,
            &input.workflow,
            &input.sha,
            &input.conclusion,
            now,
        );
        let matched = scene.apply(transition).is_some();
        scene.save(&mut work, now).await?;
        work.commit(&*self.ports.notifier).await?;
        Ok(matched)
    }
}

/// Refuses a slot the settings do not declare.
pub(crate) async fn declared(
    work: &mut Work,
    scene: &Scene,
    slot: &SlotName,
) -> Result<(), AppError> {
    if scene.catalog().get(slot).is_some() {
        return Ok(());
    }
    Err(refuse(work, SlotRefused::UnknownSlot { slot: slot.clone() }).await)
}

async fn ended(
    work: &mut Work,
    scene: &Scene,
    hold: dibs_domain::slot::SlotHold,
    slot: &SlotName,
) -> Result<EndedHold, AppError> {
    let holder: Handle = Names::load(work).await?.handle(hold.member)?;
    Ok(EndedHold {
        hold: NamedHold { hold, holder },
        line: line_of(work, scene, slot).await?,
    })
}
