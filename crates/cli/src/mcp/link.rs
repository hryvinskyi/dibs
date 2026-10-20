//! The server's connection and registered session, and the operations the tools perform on them.
//! Each operation checks its input, makes the client's call of the same purpose, and answers the
//! client's typed answer or the [`Failure`] it stands for.

use std::path::PathBuf;

use dibs_client::api::{
    Acked, ClaimRequest, Claimed, CreatedItem, DropPrRequest, EditCheck, Ended, Granted, Inbox,
    InboxRequest, Items, MarkedRead, NarrowRequest, Narrowed, PostMessage, Posted, ReadyList,
    ReviewRequest, Reviewed, StartThread, Started, Status, TakeOverRequest, TakenOver,
    ThreadDetail,
};
use dibs_client::api::{
    Escalated, EscalationRequest, HoldMode, ReleaseSlotRequest, SlotAnswer, SlotReleased, Slots,
    TakeSlotRequest, Took,
};
use dibs_domain::escalation::EscalationSubject;
use dibs_domain::ids::{ItemId, SessionId};
use dibs_domain::message::SessionLabel;

use crate::commands::check::Governed;
use crate::context::{Connection, Context};
use crate::failure::Failure;
use crate::mcp::inputs::{
    AckInput, CheckInput, ClaimInput, EscalateInput, EscalationTarget, GrantInput, InboxInput,
    ItemsInput, KeyInput, NarrowInput, NewInput, ReviewInput, SendInput, Sending, SlotInput,
    SlotTakeInput, TakeoverInput, ThreadInput,
};

/// The kind of agent the sessions of the tool server report.
const AGENT: &str = "mcp";

/// How often a waiting slot take asks again.
const SLOT_POLL: std::time::Duration = std::time::Duration::from_secs(10);

/// Either a new conversation or a message in one: what `dibs_send` answers.
#[derive(Debug, serde::Serialize)]
#[serde(untagged)]
pub enum Sent {
    /// A conversation was started.
    Started(Started),
    /// A message was posted.
    Posted(Posted),
}

/// A signed-in connection and the session the tool server registered on it.
pub struct Link {
    connection: Connection,
    session: SessionId,
    label: SessionLabel,
    cwd: PathBuf,
}

impl Link {
    /// Signs in and registers this process's session, labelled `<host>/mcp-<pid>`.
    ///
    /// # Errors
    /// The [`Failure`] of signing in or of registering.
    pub async fn open(ctx: &Context) -> Result<Self, Failure> {
        let connection = ctx.connect().await?;
        let name = format!("mcp-{}", std::process::id());
        let session = connection.register(AGENT, &name).await?;
        Ok(Self {
            connection,
            session: session.session_id,
            label: session.label,
            cwd: ctx.cwd.clone(),
        })
    }

    /// The live and stale claims and the areas held.
    ///
    /// # Errors
    /// The [`Failure`] of the call.
    pub async fn status(&self) -> Result<Status, Failure> {
        self.call(self.connection.client.status()).await
    }

    /// The work the caller can pick up now, and why the other open items wait.
    ///
    /// # Errors
    /// The [`Failure`] of the call.
    pub async fn ready(&self) -> Result<ReadyList, Failure> {
        self.call(self.connection.client.ready()).await
    }

    /// The work items that match the filter.
    ///
    /// # Errors
    /// The [`Failure`] of the call.
    pub async fn items(&self, input: ItemsInput) -> Result<Items, Failure> {
        let filter = input.filter(&self.connection.me.member.handle);
        let items = self.call(self.connection.client.items(&filter)).await?;
        Ok(Items { items })
    }

    /// Creates the item a reference names, or finds the one that exists.
    ///
    /// # Errors
    /// A [`Failure`] for an invalid input, or of the call.
    pub async fn new_item(&self, input: NewInput) -> Result<CreatedItem, Failure> {
        let item = input.item()?;
        self.call(self.connection.client.create_item(&item)).await
    }

    /// Claims an item, or renews the claim already held.
    ///
    /// # Errors
    /// A [`Failure`] for an invalid input, a refusal naming the holder, or of the call.
    pub async fn claim(&self, input: ClaimInput) -> Result<Claimed, Failure> {
        let key = input.key.key("key")?;
        let item = self.resolve(&key).await?;
        let wide = input.wide()?;
        let request = ClaimRequest {
            session_id: self.session,
            kind: input.kind(),
            areas: input.areas,
            wide,
        };
        self.call(self.connection.client.claim(item, &request))
            .await
    }

    /// Gives areas of the caller's claim up; an area another claim waited for moves to it.
    ///
    /// # Errors
    /// A [`Failure`] for an invalid input, a refusal, or of the call.
    pub async fn narrow(&self, input: NarrowInput) -> Result<Narrowed, Failure> {
        let item = self.resolve(&input.key.key("key")?).await?;
        let request = NarrowRequest { areas: input.areas };
        self.call(self.connection.client.narrow(item, &request))
            .await
    }

    /// Takes over a stale claim, telling the previous holder why.
    ///
    /// # Errors
    /// A [`Failure`] for an invalid input, a refusal when the claim is not stale, or of the call.
    pub async fn take_over(&self, input: TakeoverInput) -> Result<TakenOver, Failure> {
        let key = input.key.key("key")?;
        let reason = input.reason()?;
        let item = self.resolve(&key).await?;
        let request = TakeOverRequest {
            session_id: self.session,
            reason,
            kind: input.kind(),
            areas: input.areas,
        };
        self.call(self.connection.client.take_over(item, &request))
            .await
    }

    /// Releases the caller's claim.
    ///
    /// # Errors
    /// A [`Failure`] for an invalid input, or of the call.
    pub async fn release(&self, input: KeyInput) -> Result<Ended, Failure> {
        let item = self.resolve(&input.key.key("key")?).await?;
        self.call(self.connection.client.release(item)).await
    }

    /// Completes the caller's claim.
    ///
    /// # Errors
    /// A [`Failure`] for an invalid input, or of the call.
    pub async fn done(&self, input: KeyInput) -> Result<Ended, Failure> {
        let item = self.resolve(&input.key.key("key")?).await?;
        self.call(self.connection.client.done(item)).await
    }

    /// Records a pull request on the caller's claim, which waits in review from then on, or
    /// removes one recorded by mistake.
    ///
    /// # Errors
    /// A [`Failure`] for an invalid input, a refusal, or of the call.
    pub async fn review(&self, input: ReviewInput) -> Result<Reviewed, Failure> {
        let pr = input.pr()?;
        let item = self.resolve(&input.key.key("key")?).await?;
        if input.drop {
            return self
                .call(self.connection.client.drop_pr(item, &DropPrRequest { pr }))
                .await;
        }
        let request = ReviewRequest {
            pr,
            more: input.more,
        };
        self.call(self.connection.client.review(item, &request))
            .await
    }

    /// The verdicts on paths about to change, judged by the settings the server holds now.
    /// Nothing is reserved.
    ///
    /// # Errors
    /// A [`Failure`] for an invalid input or a directory that is no checkout, or of the call.
    pub async fn check(&self, input: CheckInput) -> Result<EditCheck, Failure> {
        let given = input.paths()?;
        let me = self.call(self.connection.client.me()).await?;
        let governed = Governed::of(&self.connection, &me)?;
        let paths = given
            .iter()
            .map(|path| governed.relative(&self.cwd, path))
            .collect::<Result<Vec<_>, _>>()?;
        let verdicts = governed
            .verdicts(&self.connection, self.session, paths)
            .await?;
        Ok(EditCheck { verdicts })
    }

    /// Lets another member edit inside an area the caller holds.
    ///
    /// # Errors
    /// A [`Failure`] for an invalid input, or of the call.
    pub async fn grant(&self, input: GrantInput) -> Result<Granted, Failure> {
        let item = self.resolve(&input.key.key("key")?).await?;
        let request = input.request(item)?;
        self.call(self.connection.client.grant(&request)).await
    }

    /// Starts a conversation or posts to one.
    ///
    /// # Errors
    /// A [`Failure`] for an invalid input, or of the call.
    pub async fn send(&self, input: SendInput) -> Result<Sent, Failure> {
        match input.sending()? {
            Sending::Reply {
                thread,
                body,
                importance,
                ack_required,
            } => {
                let message = PostMessage {
                    label: self.label.clone(),
                    body,
                    importance,
                    ack_required,
                };
                let posted = self
                    .call(self.connection.client.post_message(thread, &message))
                    .await?;
                Ok(Sent::Posted(posted))
            }
            Sending::Start {
                to,
                subject,
                item,
                body,
                importance,
                ack_required,
            } => {
                let item = match item {
                    Some(key) => Some(self.resolve(&key).await?),
                    None => None,
                };
                let start = StartThread {
                    subject,
                    to,
                    item,
                    label: self.label.clone(),
                    body,
                    importance,
                    ack_required,
                };
                let started = self
                    .call(self.connection.client.start_thread(&start))
                    .await?;
                Ok(Sent::Started(started))
            }
        }
    }

    /// The conversations waiting for the caller, and when asked the messages awaiting their
    /// acknowledgement.
    ///
    /// # Errors
    /// The [`Failure`] of the call.
    pub async fn inbox(&self, input: InboxInput) -> Result<Inbox, Failure> {
        let asked = InboxRequest {
            unread: input.unread.unwrap_or(false),
            unacked: input.unacked.unwrap_or(false),
            deliverable: false,
        };
        self.call(self.connection.client.inbox(asked)).await
    }

    /// Acknowledges a message the caller was sent.
    ///
    /// # Errors
    /// A [`Failure`] for an invalid input, or of the call.
    pub async fn ack(&self, input: AckInput) -> Result<Acked, Failure> {
        let message = input.id()?;
        self.call(self.connection.client.ack(message)).await
    }

    /// Asks the project's arbiters to settle a dispute about a work item, a slot or a
    /// conversation.
    ///
    /// # Errors
    /// A [`Failure`] for an invalid input, a refusal when the project names no other arbiter, or
    /// of the call.
    pub async fn escalate(&self, input: EscalateInput) -> Result<Escalated, Failure> {
        let reason = input.reason()?;
        let subject = match input.target()? {
            EscalationTarget::Item(key) => EscalationSubject::Item(self.resolve(&key).await?),
            EscalationTarget::Slot(slot) => EscalationSubject::Slot(slot),
            EscalationTarget::Thread(thread) => EscalationSubject::Thread(thread),
        };
        self.call(
            self.connection
                .client
                .escalate(&EscalationRequest::about(subject, reason)),
        )
        .await
    }

    /// A conversation and its messages. Showing it does not mark it read.
    ///
    /// # Errors
    /// A [`Failure`] for an invalid input, or of the call.
    pub async fn thread(&self, input: ThreadInput) -> Result<ThreadDetail, Failure> {
        let thread = input.id()?;
        self.call(self.connection.client.thread(thread)).await
    }

    /// Marks a conversation read.
    ///
    /// # Errors
    /// A [`Failure`] for an invalid input, or of the call.
    pub async fn mark_read(&self, input: ThreadInput) -> Result<MarkedRead, Failure> {
        let thread = input.id()?;
        self.call(self.connection.client.mark_read(thread)).await
    }

    /// Takes a slot, renews the caller's hold, or queues them; with a wait, asks again every 10
    /// seconds until it is theirs or the wait is over.
    ///
    /// # Errors
    /// A [`Failure`] for an invalid input, or of the call.
    pub async fn slot_take(&self, input: SlotTakeInput) -> Result<Took, Failure> {
        let slot = input.slot()?;
        let item = match &input.item {
            Some(key) => Some(self.resolve(&key.key("item")?).await?),
            None => None,
        };
        let request = TakeSlotRequest {
            session_id: self.session,
            purpose: input.purpose()?,
            item,
            mode: HoldMode::Session,
            workflow: None,
            sha: None,
        };
        self.call(crate::commands::slot::take_waiting(
            &self.connection.client,
            &slot,
            &request,
            input.wait()?,
            SLOT_POLL,
        ))
        .await
    }

    /// Releases a slot the caller holds.
    ///
    /// # Errors
    /// A [`Failure`] for an invalid input, or of the call.
    pub async fn slot_release(&self, input: SlotInput) -> Result<SlotReleased, Failure> {
        let slot = input.slot()?;
        self.call(
            self.connection
                .client
                .release_slot(&slot, &ReleaseSlotRequest::default()),
        )
        .await
    }

    /// Leaves a slot's queue, or gives up the offer to the caller.
    ///
    /// # Errors
    /// A [`Failure`] for an invalid input, or of the call.
    pub async fn slot_leave(&self, input: SlotInput) -> Result<SlotAnswer, Failure> {
        let slot = input.slot()?;
        self.call(self.connection.client.leave_slot(&slot)).await
    }

    /// Every slot and where it stands.
    ///
    /// # Errors
    /// The [`Failure`] of the call.
    pub async fn slots(&self) -> Result<Slots, Failure> {
        self.call(self.connection.client.slots()).await
    }

    async fn resolve(&self, key: &dibs_domain::item::ItemKey) -> Result<ItemId, Failure> {
        self.connection.resolve(key).await
    }

    async fn call<T>(
        &self,
        call: impl Future<Output = Result<T, dibs_client::ClientError>>,
    ) -> Result<T, Failure> {
        call.await.map_err(|error| self.connection.failed(error))
    }
}
