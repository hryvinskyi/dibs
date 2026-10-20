//! Taking a slot, renewing one's hold, or joining its group's queue.

use std::sync::Arc;

use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{ItemId, RunId, SessionId, WaiterId};
use dibs_domain::slot::{
    CommitSha, Liveness, Purpose, SlotName, TakeAnswer, TakeRequest, WorkflowName, decide_take,
};

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{Names, refuse};
use crate::ports::{Clock, EventNotifier, IdGen, Store, StoreError};
use crate::use_cases::common::{existing_item, own_session};
use crate::use_cases::slots::scene::Scene;
use crate::use_cases::slots::view::{Lines, SlotLine};
use crate::work::Work;

/// How the hold asked for would stay alive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoldMode {
    /// By the member's heartbeats, until its expiry.
    Session,
    /// By the heartbeats of the run of a command this take starts; the answer names the run.
    Run,
    /// Until the forge reports that this workflow's run for this commit finished.
    UntilRun {
        /// The workflow.
        workflow: WorkflowName,
        /// The commit.
        sha: CommitSha,
    },
}

/// The take to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakeSlotInput {
    /// The slot.
    pub slot: SlotName,
    /// The caller's session asking.
    pub session: SessionId,
    /// Why, as every member reads it.
    pub purpose: Purpose,
    /// The work item it is for, by local id.
    pub item: Option<ItemId>,
    /// How the hold stays alive.
    pub mode: HoldMode,
}

/// What the take came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TookKind {
    /// The caller holds the slot now.
    Taken,
    /// The caller held it already; its expiry moved.
    Renewed,
    /// The caller waits, at this place from 1.
    Queued {
        /// The place.
        position: usize,
    },
}

/// The take made, and where the slot stands after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TookSlot {
    /// What it came to.
    pub kind: TookKind,
    /// The slot and its group now.
    pub line: SlotLine,
    /// The run whose heartbeats keep the hold, when the hold is kept by one.
    pub run: Option<RunId>,
}

/// Takes a slot for the caller: a free slot, or one on offer to them, is theirs; their own hold is
/// renewed; otherwise they join its group's queue, once. Every member's session acts for the
/// member, so a hold taken from one session is held for the others too.
pub struct TakeSlot {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
}

impl TakeSlot {
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

    /// Makes the take.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] for a session not the
    /// caller's or an unknown item; [`AppError::Slot`] for an undeclared slot or another slot of
    /// the group the caller holds; [`AppError::Store`] when the store fails.
    pub async fn execute(&self, actor: &Actor, input: TakeSlotInput) -> Result<TookSlot, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let mut work = Work::begin(&*self.store, actor.project).await?;
        let session = own_session(&mut work, actor, input.session).await?;
        if let Some(item) = input.item {
            existing_item(&mut work, item).await?;
        }
        let mut scene = Scene::load(&mut work, &self.hot_rules, now).await?;
        let liveness = match input.mode {
            HoldMode::Session => Liveness::Session,
            HoldMode::Run => Liveness::Run {
                run: RunId(self.ids.next()),
            },
            HoldMode::UntilRun { workflow, sha } => Liveness::UntilRun { workflow, sha },
        };
        let request = TakeRequest {
            slot: input.slot.clone(),
            member: actor.member,
            session: session.id,
            host: session.host,
            purpose: input.purpose,
            item: input.item,
            liveness,
            waiter: WaiterId(self.ids.next()),
        };
        let transition = match decide_take(&scene.board, scene.catalog(), &request, now) {
            Ok(transition) => transition,
            Err(reason) => return Err(refuse(&mut work, reason).await),
        };
        let (kind, run) = match scene.apply(transition) {
            TakeAnswer::Taken { hold } => (TookKind::Taken, run_of(&hold.liveness)),
            TakeAnswer::Renewed { hold } => (TookKind::Renewed, run_of(&hold.liveness)),
            TakeAnswer::Queued { position, .. } => (TookKind::Queued { position }, None),
        };
        scene.save(&mut work, now).await?;
        let line = line_of(&mut work, &scene, &input.slot).await?;
        work.commit(&*self.notifier).await?;
        Ok(TookSlot { kind, line, run })
    }
}

fn run_of(liveness: &Liveness) -> Option<RunId> {
    match liveness {
        Liveness::Run { run } => Some(*run),
        Liveness::Session | Liveness::UntilRun { .. } => None,
    }
}

/// The named line of a declared slot of the scene.
pub(crate) async fn line_of(
    work: &mut Work,
    scene: &Scene,
    slot: &SlotName,
) -> Result<SlotLine, AppError> {
    let names = Names::load(work).await?;
    Lines {
        catalog: scene.catalog(),
        board: &scene.board,
        names: &names,
    }
    .of(slot)?
    .ok_or_else(|| {
        AppError::Store(StoreError::Corrupt {
            what: format!("the slot {slot} vanished inside one transaction"),
        })
    })
}
