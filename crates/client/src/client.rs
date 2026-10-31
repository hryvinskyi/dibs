//! The typed HTTP client of the dibs API: one method per route, each answering the route's typed
//! answer or a [`ClientError`].
//!
//! Every request carries the member's bearer token and `User-Agent: dibs/<version>`, connects
//! within 2 s and is answered within 5 s unless [`ClientOptions`] say otherwise, each sending with
//! limits of its own. Only what the options allow is sent once more: a `GET`, or a request whose
//! repeat changes nothing the first did not, when its connection could not be opened in time and
//! so nothing was sent; and a `GET` also when its connection was reset, since only a read is
//! answered the same however often the server saw it. A caller decides what any other failure means
//! for it. The event stream is the one long-lived request; it has no total limit, notices a silent
//! connection by its read limit, and reconnects where it left off.

use std::collections::VecDeque;
use std::error::Error as _;
use std::time::Duration;

use futures_util::stream::{self, BoxStream, Stream, StreamExt};
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderValue};
use reqwest::{Method, RequestBuilder};
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;

use dibs_domain::handle::Handle;
use dibs_domain::ids::{EventId, GrantId, ItemId, MessageId, SessionId, ThreadId};
use dibs_domain::item::{ExternalRef, ItemKey};
use dibs_domain::report::{DayReport, MachineId};
use dibs_domain::settings::SettingsDocument;

use crate::api::{
    Acked, AddDependency, ClaimRequest, Claimed, CreatedItem, Delivered, DependencyChange,
    DropPrRequest, EditCheck, EditCheckRequest, Ended, Enrolled, Event, EventPage, GrantAskRequest,
    GrantAsked, GrantRequest, Granted, Heartbeat, Inbox, InboxRequest, Item, ItemDetail,
    ItemFilter, Items, MarkedRead, Me, Member, MessageStatus, NarrowRequest, Narrowed, NewItem,
    NewMember, OnBranch, PostMessage, Posted, ReadyList, RegisterSession, ReviewRequest, Reviewed,
    Revoked, Session, SettingsApplied, StartThread, StartThreadBody, Started, Status,
    TakeOverRequest, TakenOver, ThreadDetail, Threads, TokenRevoked, Versions,
};
use crate::api::{
    BeatWorktreesRequest, ForceReleaseWorktreeRequest, ReleaseWorktreeRequest, SessionEnded,
    TakeWorktreeRequest, TookWorktree, WorktreeForced, WorktreeReleased, Worktrees,
    WorktreesBeaten,
};
use crate::api::{
    Escalated, EscalationRequest, FeedbackGiven, FeedbackRequest, ForceReleaseSlotRequest,
    MemberRemoved, ReleaseSlotRequest, SlotAnswer, SlotForced, SlotReleased, SlotTakenOver, Slots,
    TakeOverSlotRequest, TakeSlotRequest, ThreadDeleted, Took,
};
use crate::error::{ClientError, Response, decode};
use crate::server_url::ServerUrl;
use crate::sse::{Frame, FrameReader};
use crate::token::Token;
use dibs_domain::ids::RunId;
use dibs_domain::slot::SlotName;
use time::OffsetDateTime;

/// The `User-Agent` of every request.
pub const USER_AGENT: &str = concat!("dibs/", env!("CARGO_PKG_VERSION"));

/// The header a reconnecting stream sends with the id of the last event it received.
const LAST_EVENT_ID: &str = "last-event-id";

/// The time limits of the client's requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientOptions {
    /// How long a connection may take to open.
    pub connect_timeout: Duration,
    /// How long a request may take in all, its answer read.
    pub timeout: Duration,
    /// How long the event stream may stay silent before it is reopened; the server sends a
    /// keep-alive comment well within it.
    pub stream_silence: Duration,
    /// The first wait before the event stream reconnects; it doubles up to `stream_backoff_max`.
    pub stream_backoff: Duration,
    /// The longest wait before the event stream reconnects.
    pub stream_backoff_max: Duration,
    /// Whether a request is sent once more when its connection is lost: a `GET`, or a route
    /// documented as one whose repeat changes nothing, when its connection could not be opened in
    /// time; a `GET` alone when its connection was reset.
    pub retry_lost_connection: bool,
}

impl Default for ClientOptions {
    /// The limits of a caller someone waits on: an agent's tool call or git's hook. Nothing is
    /// retried.
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(2),
            timeout: Duration::from_secs(5),
            stream_silence: Duration::from_secs(60),
            stream_backoff: Duration::from_secs(1),
            stream_backoff_max: Duration::from_secs(30),
            retry_lost_connection: false,
        }
    }
}

impl ClientOptions {
    /// The limits of a command a person runs and watches: 5 s to connect and 20 s in all for each
    /// sending, longer than the server's own 10 s limit on a request, and a request sent once more
    /// when its connection is lost as [`ClientOptions::retry_lost_connection`] says, so that one
    /// request may take up to 40 s.
    pub fn for_person() -> Self {
        Self {
            connect_timeout: Duration::from_secs(5),
            timeout: Duration::from_secs(20),
            retry_lost_connection: true,
            ..Self::default()
        }
    }
}

/// What a request was answered with, before it is decoded.
struct Answer {
    status: u16,
    content_type: Option<String>,
    body: Vec<u8>,
}

/// A member's connection to one server.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    stream_http: reqwest::Client,
    server: ServerUrl,
    token: Token,
    options: ClientOptions,
}

#[derive(Deserialize)]
struct MembersAnswer {
    members: Vec<Member>,
}

#[derive(Serialize)]
struct HeartbeatBody<'a> {
    session_id: SessionId,
    #[serde(flatten)]
    on: Option<&'a OnBranch>,
    #[serde(skip_serializing_if = "Option::is_none")]
    machine: Option<&'a MachineId>,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    reports: &'a [DayReport],
}

#[derive(Serialize)]
struct ThreadsQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    item: Option<ItemId>,
}

#[derive(Serialize)]
struct EventsQuery {
    after: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
}

#[derive(Serialize)]
struct Empty {}

#[derive(Serialize)]
struct DeliverBody {
    session_id: SessionId,
    #[serde(
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    until: Option<OffsetDateTime>,
}

#[derive(Serialize)]
struct RunBody {
    run_id: RunId,
}

#[derive(Serialize)]
struct WorktreesQuery<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    machine: Option<&'a MachineId>,
}

/// The path of a slot's route, its name kept one segment.
fn slot_path(slot: &SlotName, rest: &str) -> String {
    format!("/v1/slots/{}{rest}", percent_encode_segment(slot.as_str()))
}

impl Client {
    /// A client of `server` that signs in with `token`.
    ///
    /// # Errors
    /// [`ClientError::Network`] when the TLS stack cannot be set up on this machine.
    pub fn new(
        server: ServerUrl,
        token: Token,
        options: ClientOptions,
    ) -> Result<Self, ClientError> {
        install_crypto_provider();
        let setup_failed = |error: reqwest::Error| {
            tracing::debug!(error = %error, "the HTTP client could not be built");
            ClientError::Network {
                server: server.to_string(),
            }
        };
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(options.connect_timeout)
            .timeout(options.timeout)
            .build()
            .map_err(setup_failed)?;
        let stream_http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(options.connect_timeout)
            .read_timeout(options.stream_silence)
            .build()
            .map_err(setup_failed)?;
        Ok(Self {
            http,
            stream_http,
            server,
            token,
            options,
        })
    }

    /// The server this client talks to.
    pub fn server(&self) -> &ServerUrl {
        &self.server
    }

    /// `GET /v1/version`: the versions the server serves and recommends. The server answers it to a
    /// client of any version.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn versions(&self) -> Result<Versions, ClientError> {
        self.send(self.request(Method::GET, "/v1/version")).await
    }

    /// `GET /v1/me`: the caller, their project, its repositories and settings.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn me(&self) -> Result<Me, ClientError> {
        self.send(self.request(Method::GET, "/v1/me")).await
    }

    /// `GET /v1/members`: the project's members, ordered by handle.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn members(&self) -> Result<Vec<Member>, ClientError> {
        let answer: MembersAnswer = self.send(self.request(Method::GET, "/v1/members")).await?;
        Ok(answer.members)
    }

    /// `POST /v1/sessions`: registers a session, or updates the one of the same label, which keeps
    /// its id.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn register_session(
        &self,
        session: &RegisterSession,
    ) -> Result<Session, ClientError> {
        self.send_repeatable(self.request(Method::POST, "/v1/sessions").json(session))
            .await
    }

    /// `POST /v1/heartbeat`: a session's sign of life, from a checkout on a branch when `on` names
    /// one, carrying what the machine `machine` counted on the days of `reports`.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn heartbeat(
        &self,
        session: SessionId,
        on: Option<&OnBranch>,
        machine: Option<&MachineId>,
        reports: &[DayReport],
    ) -> Result<Heartbeat, ClientError> {
        let body = HeartbeatBody {
            session_id: session,
            on,
            machine,
            reports,
        };
        self.send(self.request(Method::POST, "/v1/heartbeat").json(&body))
            .await
    }

    /// `GET /v1/items`: the items every given filter matches, ordered by id.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn items(&self, filter: &ItemFilter) -> Result<Vec<Item>, ClientError> {
        let answer: Items = self
            .send(self.request(Method::GET, "/v1/items").query(filter))
            .await?;
        Ok(answer.items)
    }

    /// The item carrying this reference, if one does.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn find_item(&self, reference: &ExternalRef) -> Result<Option<Item>, ClientError> {
        let filter = ItemFilter {
            external_ref: Some(reference.clone()),
            ..ItemFilter::default()
        };
        Ok(self.items(&filter).await?.into_iter().next())
    }

    /// The local id of the item a key names: a local id as it is, a reference looked up.
    ///
    /// # Errors
    /// A `not_found` [`ClientError::Refused`] when no item carries the reference; [`ClientError`].
    pub async fn resolve(&self, key: &ItemKey) -> Result<ItemId, ClientError> {
        match key {
            ItemKey::Local(id) => Ok(*id),
            ItemKey::External(reference) => match self.find_item(reference).await? {
                Some(item) => Ok(item.id),
                None => Err(ClientError::not_found(format!(
                    "There is no work item {reference}; create it with dibs new {reference}."
                ))),
            },
        }
    }

    /// `GET /v1/items/{id}`: an item and its claim.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn item(&self, id: ItemId) -> Result<ItemDetail, ClientError> {
        self.send(self.request(Method::GET, &format!("/v1/items/{id}")))
            .await
    }

    /// `POST /v1/items`: creates an item, or answers the one its reference already names.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn create_item(&self, item: &NewItem) -> Result<CreatedItem, ClientError> {
        let request = self.request(Method::POST, "/v1/items").json(item);
        if item.external_ref.is_some() {
            self.send_repeatable(request).await
        } else {
            self.send(request).await
        }
    }

    /// `POST /v1/items/{id}/claim`: claims the item, or renews the caller's own claim.
    ///
    /// # Errors
    /// [`ClientError::Conflict`] when another member is in the way; [`ClientError`].
    pub async fn claim(&self, id: ItemId, claim: &ClaimRequest) -> Result<Claimed, ClientError> {
        self.send(
            self.request(Method::POST, &format!("/v1/items/{id}/claim"))
                .json(claim),
        )
        .await
    }

    /// `POST /v1/items/{id}/claim/narrow`: gives areas of the caller's claim up; an area another
    /// claim waited for is handed over to it.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn narrow(
        &self,
        id: ItemId,
        narrow: &NarrowRequest,
    ) -> Result<Narrowed, ClientError> {
        self.send_repeatable(
            self.request(Method::POST, &format!("/v1/items/{id}/claim/narrow"))
                .json(narrow),
        )
        .await
    }

    /// `POST /v1/items/{id}/takeover`: takes over another member's stale claim.
    ///
    /// # Errors
    /// [`ClientError::Conflict`] when the claim is not stale; [`ClientError`].
    pub async fn take_over(
        &self,
        id: ItemId,
        takeover: &TakeOverRequest,
    ) -> Result<TakenOver, ClientError> {
        self.send(
            self.request(Method::POST, &format!("/v1/items/{id}/takeover"))
                .json(takeover),
        )
        .await
    }

    /// `POST /v1/items/{id}/release`: releases the caller's claim.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn release(&self, id: ItemId) -> Result<Ended, ClientError> {
        self.send(self.request(Method::POST, &format!("/v1/items/{id}/release")))
            .await
    }

    /// `POST /v1/items/{id}/done`: completes the caller's claim.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn done(&self, id: ItemId) -> Result<Ended, ClientError> {
        self.send(self.request(Method::POST, &format!("/v1/items/{id}/done")))
            .await
    }

    /// `POST /v1/items/{id}/review`: records a pull request on the caller's claim, which waits in
    /// review from then on.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn review(
        &self,
        id: ItemId,
        review: &ReviewRequest,
    ) -> Result<Reviewed, ClientError> {
        self.send(
            self.request(Method::POST, &format!("/v1/items/{id}/review"))
                .json(review),
        )
        .await
    }

    /// `POST /v1/items/{id}/review/drop`: removes a pull request the caller recorded on their
    /// claim by mistake; with none left the claim is worked on again.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn drop_pr(&self, id: ItemId, drop: &DropPrRequest) -> Result<Reviewed, ClientError> {
        self.send(
            self.request(Method::POST, &format!("/v1/items/{id}/review/drop"))
                .json(drop),
        )
        .await
    }

    /// `GET /v1/ready`: the caller's ready list and the open items left out of it.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn ready(&self) -> Result<ReadyList, ClientError> {
        self.send(self.request(Method::GET, "/v1/ready")).await
    }

    /// `POST /v1/items/{id}/dependencies`: records that the item is blocked by another.
    ///
    /// # Errors
    /// [`ClientError::Conflict`] for an edge that would close a cycle; [`ClientError`].
    pub async fn add_dependency(
        &self,
        id: ItemId,
        blocked_by: ItemId,
    ) -> Result<DependencyChange, ClientError> {
        self.send_repeatable(
            self.request(Method::POST, &format!("/v1/items/{id}/dependencies"))
                .json(&AddDependency { blocked_by }),
        )
        .await
    }

    /// `DELETE /v1/items/{id}/dependencies`: removes that the item is blocked by another.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn remove_dependency(
        &self,
        id: ItemId,
        blocked_by: ItemId,
    ) -> Result<DependencyChange, ClientError> {
        self.send_repeatable(
            self.request(Method::DELETE, &format!("/v1/items/{id}/dependencies"))
                .query(&[("blocked_by", blocked_by.get())]),
        )
        .await
    }

    /// `GET /v1/status`: live claims, stale claims and held areas.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn status(&self) -> Result<Status, ClientError> {
        self.send(self.request(Method::GET, "/v1/status")).await
    }

    /// `POST /v1/edit-check`: a verdict per path, in the order asked.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn edit_check(&self, check: &EditCheckRequest) -> Result<EditCheck, ClientError> {
        self.send(self.request(Method::POST, "/v1/edit-check").json(check))
            .await
    }

    /// `POST /v1/grants`: lets another member edit inside an area the caller holds.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn grant(&self, grant: &GrantRequest) -> Result<Granted, ClientError> {
        self.send(self.request(Method::POST, "/v1/grants").json(grant))
            .await
    }

    /// `DELETE /v1/grants/{id}`: withdraws a grant the caller gave.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn revoke_grant(&self, id: GrantId) -> Result<Revoked, ClientError> {
        self.send(self.request(Method::DELETE, &format!("/v1/grants/{id}")))
            .await
    }

    /// `POST /v1/grant-requests`: asks whoever holds an area for a grant there, for the caller's
    /// live claim on an item.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn ask_grant(&self, ask: &GrantAskRequest) -> Result<GrantAsked, ClientError> {
        self.send(self.request(Method::POST, "/v1/grant-requests").json(ask))
            .await
    }

    /// `POST /v1/threads`: starts a thread with its first message.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn start_thread(&self, start: &StartThread) -> Result<Started, ClientError> {
        self.send(
            self.request(Method::POST, "/v1/threads")
                .json(&StartThreadBody::of(start)),
        )
        .await
    }

    /// `POST /v1/threads/{id}/messages`: posts to a thread.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn post_message(
        &self,
        thread: ThreadId,
        message: &PostMessage,
    ) -> Result<Posted, ClientError> {
        self.send(
            self.request(Method::POST, &format!("/v1/threads/{thread}/messages"))
                .json(message),
        )
        .await
    }

    /// `GET /v1/threads`: the threads the caller may read, newest activity first; only those about
    /// `item`, when given.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn threads(&self, item: Option<ItemId>) -> Result<Threads, ClientError> {
        self.send(
            self.request(Method::GET, "/v1/threads")
                .query(&ThreadsQuery { item }),
        )
        .await
    }

    /// `GET /v1/inbox`: the threads waiting for the caller — only those with unread messages when
    /// asked — and, when asked, the messages awaiting their acknowledgement.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn inbox(&self, asked: InboxRequest) -> Result<Inbox, ClientError> {
        self.send(self.request(Method::GET, "/v1/inbox").query(&asked))
            .await
    }

    /// `POST /v1/messages/{id}/ack`: acknowledges a message the caller was sent.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn ack(&self, message: MessageId) -> Result<Acked, ClientError> {
        self.send_repeatable(
            self.request(Method::POST, &format!("/v1/messages/{message}/ack"))
                .json(&Empty {}),
        )
        .await
    }

    /// `POST /v1/messages/{id}/deliver`: claims the delivery of a message to the agent of the
    /// caller's session `session`; the first of their sessions to claim it wins.
    ///
    /// # Errors
    /// A `delivered` refusal when one of the caller's sessions claimed it before; [`ClientError`].
    pub async fn deliver_message(
        &self,
        message: MessageId,
        session: SessionId,
    ) -> Result<Delivered, ClientError> {
        self.send(
            self.request(Method::POST, &format!("/v1/messages/{message}/deliver"))
                .json(&DeliverBody {
                    session_id: session,
                    until: None,
                }),
        )
        .await
    }

    /// `POST /v1/slots/{name}/offer/deliver`: claims the delivery of the slot's offer to the
    /// caller, ending at `until`, to the agent of their session `session`.
    ///
    /// # Errors
    /// A `delivered` refusal when it was claimed before, `offer_gone` when the offer no longer
    /// stands; [`ClientError`].
    pub async fn deliver_offer(
        &self,
        slot: &SlotName,
        until: OffsetDateTime,
        session: SessionId,
    ) -> Result<Delivered, ClientError> {
        self.send(
            self.request(Method::POST, &slot_path(slot, "/offer/deliver"))
                .json(&DeliverBody {
                    session_id: session,
                    until: Some(until),
                }),
        )
        .await
    }

    /// `GET /v1/messages/{id}/status`: who read and acknowledged a message the caller wrote.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn message_status(&self, message: MessageId) -> Result<MessageStatus, ClientError> {
        self.send(self.request(Method::GET, &format!("/v1/messages/{message}/status")))
            .await
    }

    /// `GET /v1/threads/{id}`: a thread with its messages. Reading does not mark it read.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn thread(&self, thread: ThreadId) -> Result<ThreadDetail, ClientError> {
        self.send(self.request(Method::GET, &format!("/v1/threads/{thread}")))
            .await
    }

    /// `POST /v1/threads/{id}/read`: marks a thread read.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn mark_read(&self, thread: ThreadId) -> Result<MarkedRead, ClientError> {
        self.send(
            self.request(Method::POST, &format!("/v1/threads/{thread}/read"))
                .json(&Empty {}),
        )
        .await
    }

    /// `GET /v1/events`: one page of the log after `after` (from the start when `None`), at most
    /// `limit` events (the server's page when `None`).
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn events(
        &self,
        after: Option<EventId>,
        limit: Option<usize>,
    ) -> Result<EventPage, ClientError> {
        let query = EventsQuery {
            after: after.map_or(0, EventId::get),
            limit,
        };
        self.send(self.request(Method::GET, "/v1/events").query(&query))
            .await
    }

    /// `GET /v1/events/stream`: the log as it grows, after `after` (from the newest event on when
    /// `None`). A dropped or silent connection is reopened after a growing pause, with the id of
    /// the last event received, so nothing is missed or repeated. The stream ends after yielding a
    /// refusal: a revoked token, a forbidden caller.
    pub fn events_stream(
        &self,
        after: Option<EventId>,
    ) -> impl Stream<Item = Result<Event, ClientError>> + Send + 'static {
        let follow = Follow {
            client: self.clone(),
            last: after,
            body: None,
            reader: FrameReader::default(),
            frames: VecDeque::new(),
            pause: self.options.stream_backoff,
            ended: false,
        };
        stream::unfold(follow, Follow::next)
    }

    /// `POST /v1/admin/members`: adds a member and answers their first token.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn add_member(&self, member: &NewMember) -> Result<Enrolled, ClientError> {
        self.send(self.request(Method::POST, "/v1/admin/members").json(member))
            .await
    }

    /// `POST /v1/admin/members/{handle}/tokens`: revokes every token of a member and issues one.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn rotate_token(&self, handle: &Handle) -> Result<Enrolled, ClientError> {
        self.send(
            self.request(Method::POST, &format!("/v1/admin/members/{handle}/tokens"))
                .json(&Empty {}),
        )
        .await
    }

    /// `DELETE /v1/admin/members/{handle}`: removes a member from the project, releasing what they
    /// held.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn remove_member(&self, handle: &Handle) -> Result<MemberRemoved, ClientError> {
        self.send(self.request(Method::DELETE, &format!("/v1/admin/members/{handle}")))
            .await
    }

    /// `DELETE /v1/admin/threads/{id}`: deletes a conversation with every message in it.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn delete_thread(&self, thread: ThreadId) -> Result<ThreadDeleted, ClientError> {
        self.send(self.request(Method::DELETE, &format!("/v1/admin/threads/{thread}")))
            .await
    }

    /// `POST /v1/feedback`: what the member's agent says of a refusal, or of a conflict it resolved
    /// by hand.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn feedback(&self, feedback: &FeedbackRequest) -> Result<FeedbackGiven, ClientError> {
        self.send(self.request(Method::POST, "/v1/feedback").json(feedback))
            .await
    }

    /// `POST /v1/escalations`: asks the project's arbiters to settle a dispute.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn escalate(&self, escalation: &EscalationRequest) -> Result<Escalated, ClientError> {
        self.send(
            self.request(Method::POST, "/v1/escalations")
                .json(escalation),
        )
        .await
    }

    /// `DELETE /v1/admin/tokens/{prefix}`: revokes one token by its prefix.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn revoke_token(&self, prefix: &str) -> Result<TokenRevoked, ClientError> {
        let segment = percent_encode_segment(prefix);
        self.send_repeatable(self.request(Method::DELETE, &format!("/v1/admin/tokens/{segment}")))
            .await
    }

    /// `PUT /v1/admin/settings`: makes a settings document the project's settings.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn apply_settings(
        &self,
        settings: &SettingsDocument,
    ) -> Result<SettingsApplied, ClientError> {
        self.send_repeatable(
            self.request(Method::PUT, "/v1/admin/settings")
                .json(settings),
        )
        .await
    }

    /// `POST /v1/admin/items/{id}/force-release`: ends any member's claim on an item.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn force_release(&self, id: ItemId) -> Result<Ended, ClientError> {
        self.send(
            self.request(Method::POST, &format!("/v1/admin/items/{id}/force-release"))
                .json(&Empty {}),
        )
        .await
    }

    /// `GET /v1/slots`: every declared slot and where its group stands.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn slots(&self) -> Result<Slots, ClientError> {
        self.send(self.request(Method::GET, "/v1/slots")).await
    }

    /// `GET /v1/slots/{name}`: one slot, fresh.
    ///
    /// # Errors
    /// An `unknown_slot` refusal; [`ClientError`].
    pub async fn slot(&self, slot: &SlotName) -> Result<SlotAnswer, ClientError> {
        self.send(self.request(Method::GET, &slot_path(slot, "")))
            .await
    }

    /// `POST /v1/slots/{name}/take`: takes the slot, renews the caller's hold, or queues them.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn take_slot(
        &self,
        slot: &SlotName,
        take: &TakeSlotRequest,
    ) -> Result<Took, ClientError> {
        self.send(
            self.request(Method::POST, &slot_path(slot, "/take"))
                .json(take),
        )
        .await
    }

    /// `POST /v1/slots/{name}/release`: the holder releases the slot.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn release_slot(
        &self,
        slot: &SlotName,
        release: &ReleaseSlotRequest,
    ) -> Result<SlotReleased, ClientError> {
        self.send(
            self.request(Method::POST, &slot_path(slot, "/release"))
                .json(release),
        )
        .await
    }

    /// `POST /v1/slots/{name}/leave`: the caller leaves the queue or gives up the offer.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn leave_slot(&self, slot: &SlotName) -> Result<SlotAnswer, ClientError> {
        self.send(
            self.request(Method::POST, &slot_path(slot, "/leave"))
                .json(&Empty {}),
        )
        .await
    }

    /// `POST /v1/slots/{name}/heartbeat`: the run holding the slot is alive.
    ///
    /// # Errors
    /// A `taken_over` or `run_ended` refusal when the run's hold is gone; [`ClientError`].
    pub async fn slot_heartbeat(
        &self,
        slot: &SlotName,
        run: RunId,
    ) -> Result<SlotAnswer, ClientError> {
        self.send(
            self.request(Method::POST, &slot_path(slot, "/heartbeat"))
                .json(&RunBody { run_id: run }),
        )
        .await
    }

    /// `POST /v1/slots/{name}/takeover`: takes over another member's stalled hold.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn take_over_slot(
        &self,
        slot: &SlotName,
        takeover: &TakeOverSlotRequest,
    ) -> Result<SlotTakenOver, ClientError> {
        self.send(
            self.request(Method::POST, &slot_path(slot, "/takeover"))
                .json(takeover),
        )
        .await
    }

    /// `POST /v1/admin/slots/{name}/force-release`: an admin ends any member's hold.
    ///
    /// # Errors
    /// [`ClientError`].
    pub async fn force_release_slot(
        &self,
        slot: &SlotName,
        force: &ForceReleaseSlotRequest,
    ) -> Result<SlotForced, ClientError> {
        let path = format!(
            "/v1/admin/slots/{}/force-release",
            percent_encode_segment(slot.as_str())
        );
        self.send(self.request(Method::POST, &path).json(force))
            .await
    }

    /// `GET /v1/worktrees`: the caller's own worktree pools with a hold — only those on `machine`
    /// when it is named — and every member's holds as the board shows them.
    ///
    /// # Errors
    /// [`ClientError::ServerTooOld`] from a server that lends no worktrees; [`ClientError`].
    pub async fn worktrees(&self, machine: Option<&MachineId>) -> Result<Worktrees, ClientError> {
        let request = self
            .request(Method::GET, "/v1/worktrees")
            .query(&WorktreesQuery { machine });
        self.newer_route(self.send(request).await)
    }

    /// `POST /v1/worktrees/take`: the holder's worktree again, else the first of the names nobody
    /// holds, else a busy answer naming each hold. Sent once: a lost answer is asked again by the
    /// caller, and the same holder asking again gets the same worktree.
    ///
    /// # Errors
    /// [`ClientError::ServerTooOld`] from a server that lends no worktrees; a
    /// `worktree_names_invalid` refusal; [`ClientError`].
    pub async fn take_worktree(
        &self,
        take: &TakeWorktreeRequest,
    ) -> Result<TookWorktree, ClientError> {
        let request = self.request(Method::POST, "/v1/worktrees/take").json(take);
        self.newer_route(self.send(request).await)
    }

    /// `POST /v1/worktrees/release`: ends a hold of the caller's pool, by worktree or by holder.
    ///
    /// # Errors
    /// [`ClientError::ServerTooOld`] from a server that lends no worktrees; a `worktree_not_held`
    /// conflict; [`ClientError`].
    pub async fn release_worktree(
        &self,
        release: &ReleaseWorktreeRequest,
    ) -> Result<WorktreeReleased, ClientError> {
        let request = self
            .request(Method::POST, "/v1/worktrees/release")
            .json(release);
        self.newer_route(self.send(request).await)
    }

    /// `POST /v1/worktrees/beat`: the holders' signs of life. A repeat changes nothing the first
    /// did not, so a lost connection may send it once more.
    ///
    /// # Errors
    /// [`ClientError::ServerTooOld`] from a server that lends no worktrees; [`ClientError`].
    pub async fn beat_worktrees(
        &self,
        beats: &BeatWorktreesRequest,
    ) -> Result<WorktreesBeaten, ClientError> {
        let request = self.request(Method::POST, "/v1/worktrees/beat").json(beats);
        self.newer_route(self.send_repeatable(request).await)
    }

    /// `POST /v1/sessions/{id}/end`: the caller's session ended, and every worktree hold of it and
    /// of its agents with it.
    ///
    /// # Errors
    /// [`ClientError::ServerTooOld`] from a server that lends no worktrees; [`ClientError`].
    pub async fn end_session(&self, session: SessionId) -> Result<SessionEnded, ClientError> {
        let request = self
            .request(Method::POST, &format!("/v1/sessions/{session}/end"))
            .json(&Empty {});
        self.newer_route(self.send_repeatable(request).await)
    }

    /// `POST /v1/admin/worktrees/force-release`: an admin ends a member's hold of a worktree,
    /// named as the board names it.
    ///
    /// # Errors
    /// [`ClientError::ServerTooOld`] from a server that lends no worktrees; a `worktree_not_held`
    /// conflict; [`ClientError`].
    pub async fn force_release_worktree(
        &self,
        force: &ForceReleaseWorktreeRequest,
    ) -> Result<WorktreeForced, ClientError> {
        let request = self
            .request(Method::POST, "/v1/admin/worktrees/force-release")
            .json(force);
        self.newer_route(self.send(request).await)
    }

    /// The answer of a route newer servers added, a server without it named too old.
    fn newer_route<T>(&self, answer: Result<T, ClientError>) -> Result<T, ClientError> {
        answer.map_err(|error| error.of_newer_route(self.server.as_str()))
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.http
            .request(method, self.server.route(path))
            .bearer_auth(self.token.expose())
            .header(ACCEPT, HeaderValue::from_static("application/json"))
    }

    /// Sends a request: a `GET` as a read, anything else once.
    async fn send<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T, ClientError> {
        self.exchange(request, Repeat::Once).await
    }

    /// Sends a request whose repeat changes nothing the first did not.
    async fn send_repeatable<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
    ) -> Result<T, ClientError> {
        self.exchange(request, Repeat::Repeatable).await
    }

    async fn exchange<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        repeat: Repeat,
    ) -> Result<T, ClientError> {
        let request = request.build().map_err(|error| self.failed(&error))?;
        let repeat = if request.method() == Method::GET {
            Repeat::Read
        } else {
            repeat
        };
        let spare = if self.options.retry_lost_connection {
            request.try_clone()
        } else {
            None
        };
        let answer = match (self.attempt(request).await, spare) {
            (Err(error), Some(spare))
                if lost(&error).is_some_and(|lost| sends_again(lost, repeat)) =>
            {
                tracing::debug!(error = %error, "the connection was lost; sending the request once more");
                self.attempt(spare).await
            }
            (answer, _) => answer,
        }
        .map_err(|error| self.failed(&error))?;
        decode(Response {
            server: self.server.as_str(),
            status: answer.status,
            content_type: answer.content_type.as_deref(),
            body: &answer.body,
        })
    }

    /// One sending of `request`, its answer read whole.
    async fn attempt(&self, request: reqwest::Request) -> Result<Answer, reqwest::Error> {
        let response = self.http.execute(request).await?;
        let status = response.status().as_u16();
        let content_type = content_type(&response);
        let body = response.bytes().await?.to_vec();
        Ok(Answer {
            status,
            content_type,
            body,
        })
    }

    fn failed(&self, error: &reqwest::Error) -> ClientError {
        tracing::debug!(error = %error, "a request to the server failed");
        let server = self.server.to_string();
        if error.is_timeout() {
            ClientError::Timeout { server }
        } else {
            ClientError::Network { server }
        }
    }
}

fn content_type(response: &reqwest::Response) -> Option<String> {
    response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// Encodes a path segment so it stays one segment, whatever it holds.
fn percent_encode_segment(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// How a request's connection was lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lost {
    /// It could not be opened in time: nothing was sent.
    Unopened,
    /// It was reset before the answer was read whole: the server may have acted on the request.
    Reset,
}

/// What sending a request again would do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Repeat {
    /// A read, answered the same however often the server sees it.
    Read,
    /// A change whose repeat changes nothing more, though its answer may say "nothing changed"
    /// where the first sending changed something.
    Repeatable,
    /// A change sent once only.
    Once,
}

/// Whether a request is sent again after its connection was lost. A read always is. A
/// repeatable change is sent again only when nothing was sent, so that its answer never reports
/// as unchanged what the lost sending changed. Any other change never is.
fn sends_again(lost: Lost, repeat: Repeat) -> bool {
    match (lost, repeat) {
        (_, Repeat::Read) | (Lost::Unopened, Repeat::Repeatable) => true,
        (Lost::Reset, Repeat::Repeatable) | (_, Repeat::Once) => false,
    }
}

/// How the connection of a failed request was lost, when it was: not opened within the connect
/// limit, or reset before the answer was read whole.
fn lost(error: &reqwest::Error) -> Option<Lost> {
    if error.is_connect() && error.is_timeout() {
        return Some(Lost::Unopened);
    }
    let mut cause = error.source();
    while let Some(inner) = cause {
        if let Some(io) = inner.downcast_ref::<std::io::Error>()
            && matches!(
                io.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
            )
        {
            return Some(Lost::Reset);
        }
        cause = inner.source();
    }
    None
}

/// Makes rustls use the `ring` provider for every TLS connection of this process, unless one is
/// already chosen.
pub(crate) fn install_crypto_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
}

/// The state of an event stream between two events.
struct Follow {
    client: Client,
    last: Option<EventId>,
    body: Option<BoxStream<'static, Result<Vec<u8>, reqwest::Error>>>,
    reader: FrameReader,
    frames: VecDeque<Frame>,
    pause: Duration,
    ended: bool,
}

/// What opening the stream came to.
enum Opened {
    Body(BoxStream<'static, Result<Vec<u8>, reqwest::Error>>),
    Retry,
    Refused(ClientError),
}

impl Follow {
    async fn next(mut self) -> Option<(Result<Event, ClientError>, Self)> {
        if self.ended {
            return None;
        }
        loop {
            if let Some(frame) = self.frames.pop_front() {
                match serde_json::from_str::<Event>(&frame.data) {
                    Ok(event) => {
                        self.last = Some(event.id);
                        return Some((Ok(event), self));
                    }
                    Err(error) => {
                        tracing::debug!(error = %error, "an event of the stream is not in a shape this client reads");
                        continue;
                    }
                }
            }
            let Some(body) = self.body.as_mut() else {
                match Self::open(&self.client, self.last).await {
                    Opened::Body(body) => {
                        self.body = Some(body);
                        self.pause = self.client.options.stream_backoff;
                    }
                    Opened::Retry => self.wait().await,
                    Opened::Refused(error) => {
                        self.ended = true;
                        return Some((Err(error), self));
                    }
                }
                continue;
            };
            match body.next().await {
                Some(Ok(bytes)) => {
                    let frames = self.reader.push(&bytes);
                    self.frames.extend(frames);
                }
                Some(Err(error)) => {
                    tracing::debug!(error = %error, "the event stream broke; reopening it");
                    self.drop_connection();
                    self.wait().await;
                }
                None => {
                    self.drop_connection();
                    self.wait().await;
                }
            }
        }
    }

    fn drop_connection(&mut self) {
        self.body = None;
        self.reader = FrameReader::default();
    }

    async fn wait(&mut self) {
        tokio::time::sleep(self.pause).await;
        self.pause = (self.pause * 2).min(self.client.options.stream_backoff_max);
    }

    async fn open(client: &Client, last: Option<EventId>) -> Opened {
        let mut request = client
            .stream_http
            .get(client.server.route("/v1/events/stream"))
            .bearer_auth(client.token.expose())
            .header(ACCEPT, HeaderValue::from_static("text/event-stream"));
        if let Some(last) = last {
            request = request.header(LAST_EVENT_ID, last.get().to_string());
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                tracing::debug!(error = %error, "the event stream could not be opened");
                return Opened::Retry;
            }
        };
        let content_type = content_type(&response);
        let streaming = content_type
            .as_deref()
            .is_some_and(|value| value.starts_with("text/event-stream"));
        if response.status().is_success() && streaming {
            let body = response
                .bytes_stream()
                .map(|chunk| chunk.map(|bytes| bytes.to_vec()))
                .boxed();
            return Opened::Body(body);
        }
        let status = response.status().as_u16();
        let body = response.bytes().await.unwrap_or_default();
        let error = match decode::<AnyAnswer>(Response {
            server: client.server.as_str(),
            status,
            content_type: content_type.as_deref(),
            body: &body,
        }) {
            Ok(_) => ClientError::Unexpected {
                server: client.server.to_string(),
                status,
            },
            Err(error) => error,
        };
        if error.is_unavailable() && !matches!(error, ClientError::Outdated { .. }) {
            Opened::Retry
        } else {
            Opened::Refused(error)
        }
    }
}

/// Any JSON object: what a stream request answered instead of events.
#[derive(Deserialize)]
struct AnyAnswer {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_two_seconds_to_connect_and_five_in_all() {
        let options = ClientOptions::default();
        assert_eq!(options.connect_timeout, Duration::from_secs(2));
        assert_eq!(options.timeout, Duration::from_secs(5));
        assert!(!options.retry_lost_connection);
        assert!(USER_AGENT.starts_with("dibs/"));
        assert_eq!(USER_AGENT, format!("dibs/{}", env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn a_person_waits_longer_than_the_server_answers_and_a_lost_connection_is_tried_again() {
        let person = ClientOptions::for_person();
        assert_eq!(person.connect_timeout, Duration::from_secs(5));
        assert_eq!(person.timeout, Duration::from_secs(20));
        assert!(person.retry_lost_connection);
        let machine = ClientOptions::default();
        assert_eq!(person.stream_silence, machine.stream_silence);
        assert_eq!(person.stream_backoff, machine.stream_backoff);
        assert_eq!(person.stream_backoff_max, machine.stream_backoff_max);
    }

    #[test]
    fn a_read_is_sent_again_after_any_lost_connection_and_a_change_only_when_nothing_was_sent() {
        assert!(sends_again(Lost::Unopened, Repeat::Read));
        assert!(sends_again(Lost::Reset, Repeat::Read));
        assert!(sends_again(Lost::Unopened, Repeat::Repeatable));
        assert!(
            !sends_again(Lost::Reset, Repeat::Repeatable),
            "a reset may follow the change: its repeat would answer that nothing changed"
        );
        assert!(!sends_again(Lost::Unopened, Repeat::Once));
        assert!(!sends_again(Lost::Reset, Repeat::Once));
    }

    #[test]
    fn a_path_segment_stays_one_segment() {
        assert_eq!(percent_encode_segment("abcd1234"), "abcd1234");
        assert_eq!(percent_encode_segment("a/b?c#d"), "a%2Fb%3Fc%23d");
    }
}
