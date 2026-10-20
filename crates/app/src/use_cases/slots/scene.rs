//! The slots as one use case sees them inside its transaction: the settings, and the board with
//! what time alone changed already applied, so a read or a take between two sweeps never sees an
//! expired hold or a lapsed offer as standing.

use time::OffsetDateTime;

use dibs_domain::event::{EventKind, MessagePosted, ThreadStarted};
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{MemberId, MessageId, ThreadId};
use dibs_domain::message::{Body, Importance, Recipients, SessionLabel, Subject};
use dibs_domain::settings::ProjectSettings;
use dibs_domain::slot::{SlotBoard, SlotCatalog, Transition, decide_tick};

use crate::error::AppError;
use crate::ports::{IdGen, StoreError};
use crate::records::{MessageRecord, ThreadRecord};
use crate::use_cases::writing::{Authorship, write_message};
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

    /// Writes the board when it changed and records its events, in the order they happened;
    /// answers how many it recorded.
    pub(crate) async fn save(
        &mut self,
        work: &mut Work,
        now: OffsetDateTime,
    ) -> Result<usize, AppError> {
        if self.board != self.stored {
            let project = work.project_id();
            work.put_slot_board(project, &self.board).await?;
            self.stored = self.board.clone();
        }
        let events = std::mem::take(&mut self.events);
        let count = events.len();
        for event in events {
            work.record(now, event).await?;
        }
        Ok(count)
    }
}

/// A message to one member about a slot, from a member who acted on it, in a thread of its own:
/// `high`, asking for no acknowledgement, so it reaches them while they work.
pub(crate) struct Notice<'a> {
    pub(crate) ids: &'a dyn IdGen,
    pub(crate) from: MemberId,
    pub(crate) label: SessionLabel,
    pub(crate) to: MemberId,
    pub(crate) subject: String,
    pub(crate) body: Body,
}

impl Notice<'_> {
    /// Stores the thread and its message and records both, answering the thread.
    pub(crate) async fn send(
        self,
        work: &mut Work,
        now: OffsetDateTime,
    ) -> Result<ThreadId, AppError> {
        let project = work.project_id();
        let subject = Subject::parse(&self.subject).map_err(|_| StoreError::Corrupt {
            what: "a slot's notice has no valid subject".to_owned(),
        })?;
        let thread = ThreadRecord {
            id: ThreadId(self.ids.next()),
            subject,
            item: None,
            author: self.from,
            recipients: Recipients::Members([self.to].into()),
            created_at: now,
        };
        let message = MessageRecord {
            id: MessageId(self.ids.next()),
            thread: thread.id,
            author: self.from,
            label: self.label,
            body: self.body,
            importance: Importance::High,
            ack_required: false,
            created_at: now,
        };
        work.insert_thread(project, &thread).await?;
        write_message(work, &message, Authorship::Dibs, now).await?;
        work.record(
            now,
            EventKind::ThreadStarted(ThreadStarted {
                thread: thread.id,
                author: self.from,
                item: None,
            }),
        )
        .await?;
        work.record(
            now,
            EventKind::MessagePosted(MessagePosted {
                thread: thread.id,
                message: message.id,
                author: self.from,
            }),
        )
        .await?;
        Ok(thread.id)
    }
}
