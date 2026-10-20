//! Pushing to a session that waits for its person, for a host whose adapter can: the tool server
//! follows the project's events, and when one may bring the member something to deliver, reads
//! the inbox, claims each pressing message or offer from the server as the background sync does
//! for the hooks, and sends what it won as the host's notification.
//!
//! - **Opted in per session**, with `DIBS_PUSH_TO_IDLE=1` in the environment the host starts the
//!   tool server with, and never with `DIBS_OFF` set: a host may drop a push it was not set up to
//!   receive without a word.
//! - **One session with its hooks.** When the host names the session it started the tool server
//!   for, the tool server registers under the label the session's hooks use, so the two are one
//!   session to the server and share one ledger on this machine: the rate of showings, what was
//!   shown, what was pushed, and the stamps that say whether the session works or waits.
//! - **Only to a session that waits.** A session at work hears from its hooks between tool calls;
//!   when no stamp says which, a push waits until the hooks had their head start.
//! - **Only what may wake it**: a high or urgent message whose acknowledgement grants nothing, or a
//!   slot offer, and a few wakes per sender in a window; everything else is left to the hooks.
//! - **Never lost without a word.** Each push waits in the ledger to be seen; unless the agent reads
//!   its conversation, acknowledges it or looks at its slot, or the person submits a prompt, the
//!   session's hooks show it once after a while. A push that could not be written is theirs to show
//!   at once.
//! - The session ledger's lock is held only to read it and to record what was pushed, never across
//!   a call to the server.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rmcp::RoleServer;
use rmcp::model::{CustomNotification, ServerNotification};
use rmcp::service::Peer;
use time::OffsetDateTime;
use tokio::sync::mpsc;
use tokio::time::Instant;

use dibs_agent_host::delivery::{self, SessionState};
use dibs_agent_host::ports::{ClaimedDelivery, DeliveryLedger, Pending, Seen, SessionRef};
use dibs_agent_host::push::McpExtension;
use dibs_agent_host::{HostId, HostRegistry, SessionKey};
use dibs_client::ClientError;
use dibs_domain::delivery::{PUSH_ANNOUNCE_EVERY, RESERVED_FOR};
use dibs_domain::handle::Handle;

use crate::failure::Failure;
use crate::guard::ledger::SessionLedgers;
use crate::guard::remote::short_name;
use crate::guard::views;
use crate::mcp::link::Link;
use crate::mcp::tools::Tools;

/// The variable that opts a session into pushes: `1`, `true`, `yes` or `on`.
pub const PUSH_VAR: &str = "DIBS_PUSH_TO_IDLE";

/// How long the inbox may go unread while nothing arrives on the stream and nothing waits, so
/// that what the stream could not say — it was refused, or it was reopened without an event to
/// resume from — still goes out. A refused stream is reopened no more often.
const SWEEP_EVERY: Duration = Duration::from_secs(300);

/// How soon to try again when the server could not be reached or a session ledger was busy.
const RETRY_SOON: Duration = Duration::from_secs(5);

/// How often to look again whether a session at work began to wait, while something is pending.
const WORKING_RECHECK: Duration = Duration::from_secs(3);

/// The longest pause between two attempts to sign in while the server is away.
const OPEN_BACKOFF_MAX: Duration = Duration::from_secs(60);

/// How many times recording a push waits for a session ledger another process holds.
const RECORD_TRIES: u32 = 50;

/// How long each of those waits is.
const RECORD_PAUSE: Duration = Duration::from_millis(20);

/// What a tool server pushes with: its host's extension, and the host's id of the session it
/// serves, when the host gives it.
#[derive(Clone)]
pub struct Pusher {
    host: HostId,
    extension: Arc<dyn McpExtension>,
    pairing: Option<SessionKey>,
}

impl Pusher {
    /// The extension the tool server adds itself.
    pub fn extension(&self) -> Arc<dyn McpExtension> {
        self.extension.clone()
    }

    /// The name the tool server's session is registered under, after the machine's: the one the
    /// paired session's hooks use, when the host names that session.
    pub fn session_name(&self) -> Option<String> {
        self.pairing.as_ref().map(short_name)
    }

    /// The kind of agent its session reports: the host's id, as the hooks of its session report.
    pub fn agent(&self) -> &'static str {
        self.host.as_str()
    }

    /// The session whose ledger pushes share: the paired one, or else the tool server's own,
    /// named by its label.
    pub fn ledger_session(&self, link: &Link) -> Option<SessionRef> {
        let key = self
            .pairing
            .clone()
            .or_else(|| SessionKey::parse(link.label().as_str()))?;
        Some(SessionRef {
            host: self.host,
            key,
        })
    }
}

/// Whether the variable's value opts the session into pushes: `1`, `true`, `yes` or `on`, in any
/// case and with spaces around it. Pushing stays off with `DIBS_OFF` set, whatever this says.
pub fn opted_in(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// What `dibs mcp --host <host>` pushes with: the host's extension when its adapter has one and
/// the session opted in; nothing without a host. The session it pairs with is the one the host
/// names in the extension's variable, read through `var`.
///
/// # Errors
/// A configuration [`Failure`] for a host this binary does not link.
pub fn chosen(
    host: Option<&str>,
    registry: &HostRegistry,
    opted: bool,
    var: impl Fn(&str) -> Option<String>,
) -> Result<Option<Pusher>, Failure> {
    let Some(host) = host else {
        return Ok(None);
    };
    let adapter = registry.get(host).ok_or_else(|| {
        Failure::config(
            "unknown_host",
            format!(
                "There is no agent host '{host}' in this dibs; it knows {}.",
                registry.ids()
            ),
        )
    })?;
    if !opted || !adapter.capabilities().push_to_idle {
        return Ok(None);
    }
    let Some(extension) = adapter.push_extension() else {
        return Ok(None);
    };
    let pairing = extension
        .session_var()
        .and_then(var)
        .and_then(|id| SessionKey::parse(&id));
    Ok(Some(Pusher {
        host: adapter.id(),
        extension: Arc::from(extension),
        pairing,
    }))
}

/// Records in `session`'s ledger that `seen` showed a push reached the agent: waits a moment for a
/// ledger another process holds, and gives up quietly after that.
pub async fn saw(ledgers: &SessionLedgers, session: &SessionRef, seen: &Seen) {
    for _ in 0..RECORD_TRIES {
        if let Some(entry) = ledgers.open(session) {
            let ledger = entry.read();
            let after = delivery::after_seen(&ledger, seen);
            if after != ledger && !entry.write(&after) {
                tracing::debug!("what the agent saw could not be recorded");
            }
            return;
        }
        tokio::time::sleep(RECORD_PAUSE).await;
    }
    tracing::debug!("the session's ledger stayed busy; what the agent saw is not recorded");
}

/// Follows the project and pushes to the session until the process ends.
pub async fn follow(tools: Tools, peer: Peer<RoleServer>, pusher: Pusher) {
    let link = open(&tools).await;
    let Some(session) = pusher.ledger_session(&link) else {
        tracing::debug!("no session to share the rate of pushes with; nothing is pushed");
        return;
    };
    let me = link.connection().me.member.handle.clone();
    let ledgers = SessionLedgers::new(link.connection().state.clone());
    let mut woken = watch(&link, &me);
    let mut reopened_at = Instant::now();
    let mut attempts = Attempts::default();
    let mut announce_at = Instant::now() + std_duration(PUSH_ANNOUNCE_EVERY);
    let mut pass_at = Instant::now();
    let pushing = Pushing {
        link: &link,
        peer: &peer,
        extension: &*pusher.extension,
        ledgers: &ledgers,
        session: &session,
        me: &me,
    };
    loop {
        if woken.wait(pass_at.min(announce_at)).await {
            pass_at = Instant::now();
        }
        let now = Instant::now();
        if now >= announce_at {
            if let Err(failure) = link.announce().await {
                tracing::debug!(code = %failure.code, "the session could not announce itself");
            }
            announce_at = now + std_duration(PUSH_ANNOUNCE_EVERY);
        }
        if now >= pass_at {
            let again = pushing.pass(&mut attempts).await;
            pass_at = Instant::now() + again.unwrap_or(SWEEP_EVERY);
        }
        if !woken.open && now.duration_since(reopened_at) >= SWEEP_EVERY {
            woken = watch(&link, &me);
            reopened_at = now;
        }
    }
}

/// The connection, opened as soon as the server can be reached.
async fn open(tools: &Tools) -> Arc<Link> {
    let mut pause = RETRY_SOON;
    loop {
        match tools.link().await {
            Ok(link) => return link,
            Err(failure) => {
                tracing::debug!(code = %failure.code, "pushing waits for the server");
                tokio::time::sleep(pause).await;
                pause = (pause * 2).min(OPEN_BACKOFF_MAX);
            }
        }
    }
}

/// The events that may bring the member something, as a signal to look: the stream is followed
/// in the background and reopened with the id of the last event received, so nothing that
/// happened while it was closed is missed. A stream the server refused ends; it is opened anew at
/// the next sweep.
struct Woken {
    signals: mpsc::Receiver<()>,
    open: bool,
}

impl Woken {
    /// Waits for a signal until `at`; whether one came.
    async fn wait(&mut self, at: Instant) -> bool {
        if !self.open {
            tokio::time::sleep_until(at).await;
            return false;
        }
        match tokio::time::timeout_at(at, self.signals.recv()).await {
            Ok(Some(())) => true,
            Ok(None) => {
                self.open = false;
                false
            }
            Err(_) => false,
        }
    }
}

fn watch(link: &Link, me: &Handle) -> Woken {
    let (signal, signals) = mpsc::channel(1);
    let stream = link.connection().client.events_stream(None);
    let me = me.clone();
    tokio::spawn(async move {
        let mut stream = std::pin::pin!(stream);
        while let Some(next) = stream.next().await {
            match next {
                Ok(event) if delivery::may_bring(&event.kind, &event.members, &me) => {
                    let _ = signal.try_send(());
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::debug!(code = error.code(), "the event stream was refused");
                    return;
                }
            }
        }
    });
    Woken {
        signals,
        open: true,
    }
}

/// What this tool server tried to deliver: each claim it made, won or lost, the messages kept for
/// another session of the member until a time, and when each subject was first seen pending.
#[derive(Default)]
struct Attempts {
    claims: Vec<ClaimedDelivery>,
    kept: BTreeMap<String, OffsetDateTime>,
    first_seen: BTreeMap<String, OffsetDateTime>,
}

impl Attempts {
    fn forget_old(&mut self, now: OffsetDateTime) {
        self.claims = delivery::kept_claims(std::mem::take(&mut self.claims), now);
        self.kept.retain(|_, until| *until > now);
        self.first_seen
            .retain(|_, at| now - *at < delivery::CLAIMS_KEPT_FOR);
    }

    fn kept_now(&self, pending: &Pending, now: OffsetDateTime) -> bool {
        self.kept
            .get(&pending.subject().key())
            .is_some_and(|until| *until > now)
    }

    /// When the soonest kept message may be claimed again.
    fn next_kept(&self) -> Option<OffsetDateTime> {
        self.kept.values().min().copied()
    }
}

fn std_duration(duration: time::Duration) -> Duration {
    Duration::try_from(duration).unwrap_or(Duration::ZERO)
}

/// The soonest of two times, either of which may be absent.
fn sooner(a: Option<OffsetDateTime>, b: Option<OffsetDateTime>) -> Option<OffsetDateTime> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// One session's pushing: where it claims, sends and records.
struct Pushing<'a> {
    link: &'a Link,
    peer: &'a Peer<RoleServer>,
    extension: &'a dyn McpExtension,
    ledgers: &'a SessionLedgers,
    session: &'a SessionRef,
    me: &'a Handle,
}

/// What claiming one delivery came to.
enum Claimed {
    Won,
    Lost,
    Kept(OffsetDateTime),
    Unavailable,
}

/// Something that may be pushed, and whether the server still has to be asked for it: what the
/// session's hooks already won for it is the session's to show.
struct Candidate {
    pending: Pending,
    claimed: bool,
}

impl Pushing<'_> {
    /// Pushes what may be pushed now. Answers how soon to look again, when something waits.
    async fn pass(&self, attempts: &mut Attempts) -> Option<Duration> {
        let now = OffsetDateTime::now_utc();
        let state = delivery::session_state(&self.ledgers.activity(self.session), now);
        if state == SessionState::Working {
            return Some(WORKING_RECHECK);
        }
        let client = &self.link.connection().client;
        let inbox = match client.inbox(views::GUARD_INBOX).await {
            Ok(inbox) => inbox,
            Err(error) if error.is_unavailable() => return Some(RETRY_SOON),
            Err(error) => {
                tracing::debug!(code = error.code(), "the inbox was refused");
                return None;
            }
        };
        attempts.forget_old(now);
        let candidates = self.candidates(&inbox, attempts, now);
        let mut wait_until = attempts.next_kept();
        let ready: Vec<&Candidate> = candidates
            .iter()
            .filter(|candidate| {
                if state != SessionState::Unknown {
                    return true;
                }
                let first = attempts
                    .first_seen
                    .get(&candidate.pending.subject().key())
                    .copied()
                    .unwrap_or(now);
                let after = first + delivery::HOOKS_FIRST;
                if after > now {
                    wait_until = sooner(wait_until, Some(after));
                    return false;
                }
                true
            })
            .collect();
        let Some(ledger) = self.ledgers.open(self.session).map(|entry| entry.read()) else {
            return Some(RETRY_SOON);
        };
        if let Some(at) = delivery::next_showing(&ledger, now) {
            return Some(std_duration(at - now));
        }
        let pendings: Vec<Pending> = ready
            .iter()
            .map(|candidate| candidate.pending.clone())
            .collect();
        let picks = delivery::to_push(&ledger, &pendings, self.me, now);
        if picks.len() < pendings.len() && !picks.is_empty() {
            wait_until = sooner(wait_until, Some(now + delivery::DELIVER_EVERY));
        }
        let mut written = Vec::new();
        let mut unwritten = Vec::new();
        for pending in picks {
            let claimed = ready
                .iter()
                .any(|candidate| candidate.pending == pending && candidate.claimed);
            let answer = if claimed {
                Claimed::Won
            } else {
                self.claim(&pending).await
            };
            match answer {
                Claimed::Won => {
                    attempts
                        .claims
                        .push(self.attempt(pending.clone(), true, now));
                    if self.send(&pending).await {
                        written.push(pending);
                    } else {
                        unwritten.push(pending);
                    }
                }
                Claimed::Lost => attempts.claims.push(self.attempt(pending, false, now)),
                Claimed::Kept(until) => {
                    attempts.kept.insert(pending.subject().key(), until);
                    wait_until = sooner(wait_until, Some(until));
                }
                Claimed::Unavailable => {
                    wait_until = sooner(wait_until, Some(now + time::Duration::seconds(5)));
                    break;
                }
            }
        }
        if !written.is_empty() || !unwritten.is_empty() {
            self.record(&written, &unwritten, now).await;
        }
        wait_until
            .map(|at| std_duration(at - OffsetDateTime::now_utc()) + Duration::from_millis(50))
    }

    /// What may be pushed: what the session's hooks won and the session neither showed nor was
    /// pushed, then what the server still has to deliver to the member and this tool server did
    /// not try yet, each noted the first time it is seen.
    fn candidates(
        &self,
        inbox: &dibs_client::api::Inbox,
        attempts: &mut Attempts,
        now: OffsetDateTime,
    ) -> Vec<Candidate> {
        let mut candidates: Vec<Candidate> = Vec::new();
        if let Some(cached) = self.link.connection().state.inbox() {
            let view = views::inbox(&cached.inbox, &cached.claims);
            for pending in delivery::won_by(&view, self.session) {
                candidates.push(Candidate {
                    pending,
                    claimed: true,
                });
            }
        }
        let mut view = views::inbox(inbox, &[]);
        view.claims = attempts.claims.clone();
        for pending in delivery::to_claim(&view, self.me) {
            let known = candidates
                .iter()
                .any(|candidate| candidate.pending.subject() == pending.subject());
            if !known && !attempts.kept_now(&pending, now) {
                candidates.push(Candidate {
                    pending,
                    claimed: false,
                });
            }
        }
        for candidate in &candidates {
            attempts
                .first_seen
                .entry(candidate.pending.subject().key())
                .or_insert(now);
        }
        candidates
    }

    async fn claim(&self, pending: &Pending) -> Claimed {
        let client = &self.link.connection().client;
        let session = self.link.session();
        let answer = match pending {
            Pending::Message(message) => client.deliver_message(message.message, session).await,
            Pending::Offer(offer) => {
                client
                    .deliver_offer(&offer.slot, offer.until, session)
                    .await
            }
        };
        match answer {
            Ok(_) => Claimed::Won,
            Err(error) => judged(&error),
        }
    }

    /// Writes the push to the host; whether it was written.
    async fn send(&self, pending: &Pending) -> bool {
        let pushed = self.extension.notification(pending);
        let notification = ServerNotification::CustomNotification(CustomNotification::new(
            pushed.method,
            Some(pushed.params),
        ));
        match self.peer.send_notification(notification).await {
            Ok(()) => true,
            Err(error) => {
                tracing::debug!(error = %error, "a push could not be written");
                false
            }
        }
    }

    /// Records what was pushed in the session's ledger, read afresh under its lock, waiting a
    /// moment for a ledger another process holds.
    async fn record(&self, written: &[Pending], unwritten: &[Pending], now: OffsetDateTime) {
        for _ in 0..RECORD_TRIES {
            if let Some(entry) = self.ledgers.open(self.session) {
                let after = delivery::after_pushing(&entry.read(), written, unwritten, now);
                if !entry.write(&after) {
                    tracing::debug!("what was pushed could not be recorded");
                }
                return;
            }
            tokio::time::sleep(RECORD_PAUSE).await;
        }
        tracing::debug!("the session's ledger stayed busy; what was pushed is not recorded");
    }

    fn attempt(&self, pending: Pending, won: bool, now: OffsetDateTime) -> ClaimedDelivery {
        ClaimedDelivery {
            pending,
            host: self.session.host.as_str().to_owned(),
            session: self.session.key.clone(),
            won,
            at: now,
        }
    }
}

/// What a refused claim means for pushing: kept for another session until the time the server
/// gives, gone for good, or to be tried again once the server answers.
fn judged(error: &ClientError) -> Claimed {
    if error.is_unavailable() {
        Claimed::Unavailable
    } else if error.code() == "reserved" {
        let until = error
            .details()
            .and_then(|details| details.until)
            .unwrap_or_else(|| OffsetDateTime::now_utc() + RESERVED_FOR);
        Claimed::Kept(until)
    } else {
        tracing::debug!(code = error.code(), "a delivery went to another session");
        Claimed::Lost
    }
}

#[cfg(test)]
mod tests {
    use dibs_agent_host::neutral::Neutral;

    use super::*;

    #[test]
    fn a_session_opts_in_with_the_usual_words() {
        for on in ["1", "true", "yes", "on", " ON "] {
            assert!(opted_in(Some(on)), "{on}");
        }
        for off in [None, Some(""), Some("0"), Some("off"), Some("no")] {
            assert!(!opted_in(off), "{off:?}");
        }
    }

    #[test]
    fn nothing_is_pushed_without_a_host_an_opt_in_or_a_host_that_can_wake_a_session() {
        let registry = HostRegistry::new(vec![Box::new(Neutral)]);
        let none = |_: &str| None;
        assert!(chosen(None, &registry, true, none).unwrap().is_none());
        assert!(
            chosen(Some("neutral"), &registry, true, none)
                .unwrap()
                .is_none(),
            "the neutral host cannot wake a session"
        );
        let unknown = chosen(Some("elsewhere"), &registry, true, none)
            .err()
            .unwrap();
        assert_eq!(unknown.code, "unknown_host");
        assert!(unknown.message.contains("neutral"), "{}", unknown.message);
    }
}
