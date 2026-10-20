//! The slots as one use case sees them inside its transaction: the settings, and the board with
//! what time alone changed already applied, so a read or a take between two sweeps never sees an
//! expired hold or a lapsed offer as standing.

use time::OffsetDateTime;

use dibs_domain::event::EventKind;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{MemberId, ThreadId};
use dibs_domain::message::{Author, Body, Importance, Recipients};
use dibs_domain::settings::ProjectSettings;
use dibs_domain::slot::{
    SlotBoard, SlotCatalog, SlotNotice, SlotNoticeKind, Transition, decide_tick, notices_owed,
};

use crate::error::AppError;
use crate::ports::IdGen;
use crate::use_cases::writing::{Posted, SystemPost, system_body};
use crate::work::Work;

/// The settings and the slots' board, and the events its changes record.
pub(crate) struct Scene {
    pub(crate) settings: ProjectSettings,
    pub(crate) board: SlotBoard,
    stored: SlotBoard,
    events: Vec<EventKind>,
}

impl Scene {
    /// Reads the settings and the board, and applies what time alone changed by `now`.
    pub(crate) async fn load(
        work: &mut Work,
        hot_rules: &HotRuleRegistry,
        now: OffsetDateTime,
    ) -> Result<Self, AppError> {
        let settings = work.project_settings(hot_rules).await?;
        let project = work.project_id();
        let stored = work.slot_board(project).await?;
        let mut scene = Self {
            settings,
            board: stored.clone(),
            stored,
            events: Vec::new(),
        };
        let ticked = decide_tick(&scene.board, scene.catalog(), now);
        scene.apply(ticked);
        Ok(scene)
    }

    /// The declared slots.
    pub(crate) fn catalog(&self) -> &SlotCatalog {
        self.settings.slots()
    }

    /// Takes a transition's board and events, answering its answer.
    pub(crate) fn apply<A>(&mut self, transition: Transition<A>) -> A {
        self.board = transition.board;
        self.events.extend(transition.events);
        transition.answer
    }

    /// Writes the board when it changed and records its events, in the order they happened, then
    /// tells each member whose hold expired or whose offer lapsed among them; answers how many
    /// events it recorded. Every change of the board is saved here, so a member is told once,
    /// whichever use case or sweep found the change.
    pub(crate) async fn save(
        &mut self,
        work: &mut Work,
        ids: &dyn IdGen,
        now: OffsetDateTime,
    ) -> Result<usize, AppError> {
        if self.board != self.stored {
            let project = work.project_id();
            work.put_slot_board(project, &self.board).await?;
            self.stored = self.board.clone();
        }
        let events = std::mem::take(&mut self.events);
        let count = events.len();
        for event in &events {
            work.record(now, event.clone()).await?;
        }
        tell_owed(work, ids, &events, now).await?;
        Ok(count)
    }
}

/// A message to one member about a slot, which dibs writes for a member who acted on it, in a
/// thread of its own: `high`, asking for no acknowledgement, so it reaches them while they work.
pub(crate) struct Notice<'a> {
    pub(crate) ids: &'a dyn IdGen,
    pub(crate) from: MemberId,
    pub(crate) to: MemberId,
    pub(crate) subject: String,
    /// What happened, in dibs's words.
    pub(crate) lead: String,
    /// The reason the member gave.
    pub(crate) reason: Body,
}

impl Notice<'_> {
    /// Stores the thread and its message and records both, answering the thread.
    pub(crate) async fn send(
        self,
        work: &mut Work,
        now: OffsetDateTime,
    ) -> Result<ThreadId, AppError> {
        let posted = SystemPost {
            ids: Posted::next(self.ids),
            subject: self.subject,
            item: None,
            on_behalf_of: Author::Member(self.from),
            to: Recipients::Members([self.to].into()),
            body: system_body(&self.lead, Some(("Their reason", self.reason.as_str())))?,
            importance: Importance::High,
            ack_required: false,
        }
        .send(work, now)
        .await?;
        Ok(posted.thread)
    }
}

/// Tells each member what time alone ended of theirs among the changes `events` record: their hold
/// expired, or the slot offered to them lapsed. dibs writes it in nobody's name, since nobody acted.
async fn tell_owed(
    work: &mut Work,
    ids: &dyn IdGen,
    events: &[EventKind],
    now: OffsetDateTime,
) -> Result<(), AppError> {
    for notice in notices_owed(events) {
        let (subject, body) = owed_text(&notice);
        SystemPost {
            ids: Posted::next(ids),
            subject,
            item: None,
            on_behalf_of: Author::System,
            to: Recipients::Members([notice.member].into()),
            body: system_body(&body, None)?,
            importance: Importance::High,
            ack_required: false,
        }
        .send(work, now)
        .await?;
    }
    Ok(())
}

/// The subject and text of a notice time alone owes.
fn owed_text(notice: &SlotNotice) -> (String, String) {
    let slot = &notice.slot;
    match notice.kind {
        SlotNoticeKind::HoldExpired => (
            format!("Your hold on {slot} expired"),
            format!(
                "Your hold on {slot} reached its expiry and ended. Nobody took it from you: it was not renewed in time. If you still need it, take it again (dibs_slot_take {slot}, or dibs slot take {slot}); it may be offered to someone waiting first."
            ),
        ),
        SlotNoticeKind::OfferLapsed => (
            format!("The offer of {slot} to you lapsed"),
            format!(
                "The slot {slot} was offered to you and the offer lapsed untaken, so it passed on and you are out of its queue. If you still need it, take it again (dibs_slot_take {slot}, or dibs slot take {slot}) to queue for it."
            ),
        ),
    }
}
