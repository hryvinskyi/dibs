//! The event stream: a project's log sent to one follower as server-sent events.
//!
//! The stream reads the log from the store by cursor, one page at a time, through the follower's
//! own use case, so each page holds only what that follower may see and no transaction stays open
//! between pages. The notifier only wakes it: a wake-up for the follower's project with an event
//! past its cursor makes it read again, a lost or lagged wake-up costs nothing but a read. It reads
//! a page only when the connection has taken every frame of the one before, so a slow reader holds
//! one page of frames at most; a member may hold a few streams open and the service a bounded
//! number in all. Between pages it sends a comment line at a fixed interval, so idle
//! proxies keep the connection, and it ends when the server shuts down, when the follower's token
//! stops working, or when the store fails.

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, broadcast, watch};
use tokio::time::{Interval, MissedTickBehavior};

use dibs_app::use_cases::{EventCursor, FollowPage, UseCases};
use dibs_app::{Actor, AppError};
use dibs_domain::ids::{EventId, MemberId, ProjectId};

use crate::adapters::{BroadcastNotifier, Wakeup};
use crate::error::ApiError;
use crate::views::EventView;

/// The comment a stream sends while there is nothing to send.
pub const HEARTBEAT: &str = ": keep-alive\n\n";

/// How a stream paces itself and how many may be open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamOptions {
    /// How often an idle stream sends [`HEARTBEAT`].
    pub heartbeat: Duration,
    /// How many streams may be open at once; one more is refused until another closes.
    pub max_open: usize,
    /// How many of them one member may hold open.
    pub max_per_member: usize,
    /// How many events one read of the log takes at most.
    pub page: usize,
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self {
            heartbeat: Duration::from_secs(25),
            max_open: 128,
            max_per_member: 8,
            page: 25,
        }
    }
}

/// Tells every open stream that the server is stopping.
#[derive(Debug)]
pub struct Shutdown {
    sender: watch::Sender<bool>,
}

impl Shutdown {
    /// A shutdown not yet given.
    pub fn new() -> Self {
        let (sender, _) = watch::channel(false);
        Self { sender }
    }

    /// The signal the streams wait on.
    pub fn signal(&self) -> StopSignal {
        StopSignal(self.sender.subscribe())
    }

    /// Ends every stream, open now or opened later.
    pub fn stop(&self) {
        self.sender.send_replace(true);
    }
}

impl Default for Shutdown {
    fn default() -> Self {
        Self::new()
    }
}

/// What a stream waits on to learn the server is stopping.
#[derive(Debug, Clone)]
pub struct StopSignal(watch::Receiver<bool>);

impl StopSignal {
    /// Resolves once the shutdown is given. If the [`Shutdown`] is dropped without being given, it
    /// never resolves.
    pub async fn stopped(&mut self) {
        loop {
            if *self.0.borrow_and_update() {
                return;
            }
            if self.0.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

/// How many streams each member holds open. A member with none has no entry, so the map holds one
/// entry per member with an open stream at most.
#[derive(Debug, Default)]
pub struct OpenPerMember {
    counts: Mutex<HashMap<(ProjectId, MemberId), usize>>,
}

/// One member's place among the open streams, given back when it is dropped.
#[derive(Debug)]
pub struct MemberSlot {
    owner: Arc<OpenPerMember>,
    key: (ProjectId, MemberId),
}

impl OpenPerMember {
    /// A place for one more stream of the member, unless they hold `max` already.
    pub fn take(
        self: &Arc<Self>,
        project: ProjectId,
        member: MemberId,
        max: usize,
    ) -> Option<MemberSlot> {
        let key = (project, member);
        let mut counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        let count = counts.entry(key).or_insert(0);
        if *count >= max {
            if *count == 0 {
                counts.remove(&key);
            }
            return None;
        }
        *count += 1;
        Some(MemberSlot {
            owner: self.clone(),
            key,
        })
    }

    /// How many streams the member holds open.
    pub fn count(&self, project: ProjectId, member: MemberId) -> usize {
        self.counts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(project, member))
            .copied()
            .unwrap_or(0)
    }

    /// How many members hold a stream open.
    pub fn members(&self) -> usize {
        self.counts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

impl Drop for MemberSlot {
    fn drop(&mut self) {
        let mut counts = self
            .owner
            .counts
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = counts.get_mut(&self.key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.remove(&self.key);
            }
        }
    }
}

/// What the routes of the event stream share: the wake-ups, the shutdown, the pace, and the count
/// of open streams, in all and per member.
#[derive(Debug, Clone)]
pub struct EventStreams {
    notifier: Arc<BroadcastNotifier>,
    stop: StopSignal,
    options: StreamOptions,
    open: Arc<Semaphore>,
    per_member: Arc<OpenPerMember>,
}

impl EventStreams {
    /// Streams woken by this notifier and ended by this signal.
    pub fn new(notifier: Arc<BroadcastNotifier>, stop: StopSignal, options: StreamOptions) -> Self {
        Self {
            notifier,
            stop,
            options,
            open: Arc::new(Semaphore::new(options.max_open.max(1))),
            per_member: Arc::new(OpenPerMember::default()),
        }
    }

    /// Opens a stream for the actor from `from`. The stream is subscribed to wake-ups before its
    /// first read, so nothing appended after that read goes unnoticed.
    ///
    /// # Errors
    /// `too_many_streams` when the member holds as many streams open as one member may, or the
    /// service as many as it allows; the answer for the use case's refusal of the first read
    /// (`forbidden` for an integration).
    pub async fn open(
        &self,
        app: Arc<UseCases>,
        actor: Actor,
        from: EventCursor,
    ) -> Result<Response, ApiError> {
        let slot = self
            .per_member
            .take(actor.project, actor.member, self.options.max_per_member)
            .ok_or_else(|| ApiError::too_many_member_streams(self.options.max_per_member))?;
        let permit = self
            .open
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::too_many_streams())?;
        let wakeups = self.notifier.subscribe();
        let first = app
            .follow_events
            .execute(&actor, from, self.options.page)
            .await?;
        let start = tokio::time::Instant::now() + self.options.heartbeat;
        let mut heartbeat = tokio::time::interval_at(start, self.options.heartbeat);
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut follower = Follower {
            app,
            actor,
            cursor: EventCursor::Start,
            pending: VecDeque::new(),
            more: false,
            wakeups,
            stop: self.stop.clone(),
            heartbeat,
            page: self.options.page,
            _permit: permit,
            _slot: slot,
        };
        follower.take(first);
        let body = Body::from_stream(futures_util::stream::unfold(follower, Follower::next));
        Ok(response(body))
    }
}

/// The answer that carries a stream: `text/event-stream`, never cached or transformed on the way.
fn response(body: Body) -> Response {
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("no-cache, no-transform"),
            ),
            (
                header::HeaderName::from_static("x-accel-buffering"),
                HeaderValue::from_static("no"),
            ),
        ],
        body,
    )
        .into_response()
}

/// One event as a server-sent event: its id, then its JSON as `data` lines, then a blank line.
/// A line break inside the data starts another `data` line, so no text can end the event early.
pub fn frame(id: EventId, data: &str) -> String {
    let mut text = format!("id: {id}\n");
    for line in data.split(['\n', '\r']) {
        text.push_str("data: ");
        text.push_str(line);
        text.push('\n');
    }
    text.push('\n');
    text
}

/// Whether a wake-up says something the follower has not read yet was appended to its log.
pub fn is_news(wakeup: Wakeup, actor: &Actor, read_up_to: Option<EventId>) -> bool {
    wakeup.project == actor.project && read_up_to < Some(wakeup.last)
}

/// One open stream.
struct Follower {
    app: Arc<UseCases>,
    actor: Actor,
    cursor: EventCursor,
    pending: VecDeque<Bytes>,
    more: bool,
    wakeups: broadcast::Receiver<Wakeup>,
    stop: StopSignal,
    heartbeat: Interval,
    page: usize,
    _permit: OwnedSemaphorePermit,
    _slot: MemberSlot,
}

impl Follower {
    /// The next frame to send, or `None` when the stream ends.
    async fn next(mut self) -> Option<(Result<Bytes, Infallible>, Self)> {
        loop {
            if let Some(frame) = self.pending.pop_front() {
                return Some((Ok(frame), self));
            }
            if self.more {
                self.read().await.ok()?;
                continue;
            }
            tokio::select! {
                biased;
                () = self.stop.stopped() => return None,
                woken = self.wakeups.recv() => match woken {
                    Ok(wakeup) => {
                        self.more = is_news(wakeup, &self.actor, self.read_up_to());
                    }
                    Err(RecvError::Lagged(_)) => self.more = true,
                    Err(RecvError::Closed) => return None,
                },
                _ = self.heartbeat.tick() => {
                    return Some((Ok(Bytes::from_static(HEARTBEAT.as_bytes())), self));
                }
            }
        }
    }

    /// The id of the last event read, if any was.
    fn read_up_to(&self) -> Option<EventId> {
        match self.cursor {
            EventCursor::After(id) => Some(id),
            EventCursor::Start | EventCursor::Latest => None,
        }
    }

    /// Reads the next page into the frames to send.
    async fn read(&mut self) -> Result<(), AppError> {
        match self
            .app
            .follow_events
            .execute(&self.actor, self.cursor, self.page)
            .await
        {
            Ok(page) => {
                self.take(page);
                Ok(())
            }
            Err(error) => {
                if matches!(error, AppError::Unauthorized) {
                    tracing::info!("an event stream ended: its token no longer works");
                } else {
                    tracing::error!(code = error.code(), error = ?error, "an event stream failed");
                }
                Err(error)
            }
        }
    }

    /// Queues a page's events as frames and moves the cursor past it.
    fn take(&mut self, page: FollowPage) {
        for followed in page.events {
            let id = followed.event.event.id;
            match serde_json::to_string(&EventView::followed(followed)) {
                Ok(data) => self.pending.push_back(Bytes::from(frame(id, &data))),
                Err(error) => {
                    tracing::error!(event = %id, error = %error, "an event could not be written");
                }
            }
        }
        self.cursor = page.next.map_or(EventCursor::Start, EventCursor::After);
        self.more = page.more;
    }
}

#[cfg(test)]
mod tests {
    use dibs_app::token::TokenHash;
    use dibs_domain::ids::{MemberId, ProjectId};
    use dibs_domain::member::Role;

    use super::*;

    #[test]
    fn an_event_is_its_id_then_its_data_then_a_blank_line() {
        assert_eq!(
            frame(EventId(42), r#"{"kind":"claimed"}"#),
            "id: 42\ndata: {\"kind\":\"claimed\"}\n\n"
        );
    }

    #[test]
    fn a_line_break_in_the_data_cannot_end_the_event() {
        let framed = frame(EventId(7), "first\n\nid: 99\r\nlast");
        assert_eq!(
            framed,
            "id: 7\ndata: first\ndata: \ndata: id: 99\ndata: \ndata: last\n\n"
        );
        assert_eq!(framed.matches("\n\n").count(), 1);
        assert!(framed.ends_with("\n\n"));
    }

    #[test]
    fn the_heartbeat_is_a_comment() {
        assert!(HEARTBEAT.starts_with(':'));
        assert!(HEARTBEAT.ends_with("\n\n"));
        assert_eq!(HEARTBEAT.matches('\n').count(), 2);
    }

    #[test]
    fn only_a_wake_up_past_the_cursor_in_the_own_project_is_news() {
        let actor = Actor {
            project: ProjectId(1),
            member: MemberId(2),
            role: Role::Member,
            token: TokenHash::new("h".to_owned()),
        };
        let wakeup = |project, last| Wakeup {
            project: ProjectId(project),
            last: EventId(last),
        };
        assert!(is_news(wakeup(1, 5), &actor, None));
        assert!(is_news(wakeup(1, 5), &actor, Some(EventId(4))));
        assert!(!is_news(wakeup(1, 5), &actor, Some(EventId(5))));
        assert!(!is_news(wakeup(1, 5), &actor, Some(EventId(9))));
        assert!(!is_news(wakeup(2, 50), &actor, Some(EventId(4))));
    }

    #[test]
    fn a_member_holds_up_to_the_limit_and_a_closed_stream_frees_a_place() {
        let open = Arc::new(OpenPerMember::default());
        let (project, alice, bob) = (ProjectId(1), MemberId(2), MemberId(3));
        let mut held: Vec<MemberSlot> = (0..3)
            .map(|_| open.take(project, alice, 3).unwrap())
            .collect();
        assert!(open.take(project, alice, 3).is_none());
        assert_eq!(open.count(project, alice), 3);
        let bobs = open.take(project, bob, 3).unwrap();
        assert!(open.take(ProjectId(9), alice, 3).is_some());
        held.pop();
        assert_eq!(open.count(project, alice), 2);
        held.push(open.take(project, alice, 3).unwrap());
        drop(held);
        drop(bobs);
        assert_eq!(open.members(), 0);
        assert!(open.take(project, alice, 0).is_none());
        assert_eq!(open.members(), 0);
    }

    #[tokio::test]
    async fn a_stop_signal_resolves_once_given_and_for_late_waiters_too() {
        let shutdown = Shutdown::new();
        let mut early = shutdown.signal();
        let waiting = tokio::spawn(async move { early.stopped().await });
        shutdown.stop();
        tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .unwrap()
            .unwrap();
        let mut late = shutdown.signal();
        tokio::time::timeout(Duration::from_secs(5), late.stopped())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_dropped_shutdown_stops_nothing() {
        let shutdown = Shutdown::new();
        let mut signal = shutdown.signal();
        drop(shutdown);
        let waited = tokio::time::timeout(Duration::from_millis(50), signal.stopped()).await;
        assert!(waited.is_err());
    }
}
