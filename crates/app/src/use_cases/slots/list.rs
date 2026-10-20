//! Reading where the slots stand, and the sweep that applies what time changes when nobody asks.

use std::sync::Arc;

use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::ProjectId;
use dibs_domain::slot::{SlotName, SlotRefused};

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::refuse;
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::use_cases::slots::scene::Scene;
use crate::use_cases::slots::take::line_of;
use crate::use_cases::slots::view::{SlotLine, scene_lines};
use crate::work::Work;

/// Where every declared slot stands: its group's hold, queue and offer. An integration may read
/// them, to watch the runs holds wait for. Reading applies what time changed first, and records it.
pub struct Slots {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
}

impl Slots {
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

    /// Every declared slot, in declaration order.
    ///
    /// # Errors
    /// [`AppError::Store`] when the store fails.
    pub async fn execute(&self, actor: &Actor) -> Result<Vec<SlotLine>, AppError> {
        let now = self.clock.now();
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let mut scene = Scene::load(&mut work, &self.hot_rules, now).await?;
        scene.save(&mut work, &*self.ids, now).await?;
        let lines = scene_lines(&mut work, &scene).await?;
        work.commit(&*self.notifier).await?;
        Ok(lines)
    }

    /// One slot.
    ///
    /// # Errors
    /// [`AppError::Slot`] for an undeclared slot; [`AppError::Store`] when the store fails.
    pub async fn one(&self, actor: &Actor, slot: &SlotName) -> Result<SlotLine, AppError> {
        let now = self.clock.now();
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let mut scene = Scene::load(&mut work, &self.hot_rules, now).await?;
        if scene.catalog().get(slot).is_none() {
            return Err(refuse(&mut work, SlotRefused::UnknownSlot { slot: slot.clone() }).await);
        }
        scene.save(&mut work, &*self.ids, now).await?;
        let line = line_of(&mut work, &scene, slot).await?;
        work.commit(&*self.notifier).await?;
        Ok(line)
    }
}

/// What one sweep did.
#[derive(Debug, Default)]
pub struct SweepReport {
    /// How many projects had a hold, a waiter or an offer.
    pub projects: usize,
    /// How many events the sweep recorded.
    pub events: usize,
    /// The projects the sweep could not apply, each with why; the others were applied all the same.
    pub failed: Vec<(ProjectId, AppError)>,
}

/// Applies what time changed to the slots of every project that has a hold, a waiter or an offer,
/// each project in a transaction of its own: holds expire or stall, offers lapse, silent waiters
/// lose their place, and freed groups are offered on — so the first waiter is offered a slot even
/// when nobody touches it. The service runs it on a schedule; it acts for nobody.
pub struct TickSlots {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
}

impl TickSlots {
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

    /// Sweeps every busy project once.
    ///
    /// # Errors
    /// [`AppError::Store`] when the busy projects cannot be listed; a project that fails is in the
    /// report instead.
    pub async fn execute(&self) -> Result<SweepReport, AppError> {
        let mut tx = self.store.begin().await?;
        let projects = tx.busy_slot_projects().await?;
        drop(tx);
        let mut report = SweepReport {
            projects: projects.len(),
            ..SweepReport::default()
        };
        for project in projects {
            match self.sweep(project).await {
                Ok(events) => report.events += events,
                Err(error) => report.failed.push((project, error)),
            }
        }
        Ok(report)
    }

    async fn sweep(&self, project: ProjectId) -> Result<usize, AppError> {
        let now = self.clock.now();
        let mut work = Work::begin_unattended(&*self.store, project).await?;
        let mut scene = Scene::load(&mut work, &self.hot_rules, now).await?;
        let events = scene.save(&mut work, &*self.ids, now).await?;
        work.commit(&*self.notifier).await?;
        Ok(events)
    }
}
