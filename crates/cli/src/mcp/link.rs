//! The server's connection and registered session, and the operations the tools perform on them.
//! Each operation checks its input, makes the client's call of the same purpose, and answers the
//! client's typed answer or the [`Failure`] it stands for.

use std::path::PathBuf;

use dibs_client::api::{
    Acked, ClaimRequest, Claimed, CreatedItem, EditCheck, Ended, Granted, Inbox, InboxRequest,
    Items, MarkedRead, PostMessage, Posted, ReadyList, StartThread, Started, Status,
    TakeOverRequest, TakenOver, ThreadDetail,
};
use dibs_domain::ids::{ItemId, SessionId};
use dibs_domain::message::SessionLabel;

use crate::commands::check::Governed;
use crate::context::{Connection, Context};
use crate::failure::Failure;
use crate::mcp::inputs::{
    AckInput, CheckInput, ClaimInput, GrantInput, InboxInput, ItemsInput, KeyInput, NewInput,
    SendInput, Sending, TakeoverInput, ThreadInput,
};

/// The kind of agent the sessions of the tool server report.
const AGENT: &str = "mcp";

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
            areas: input.areas,
            wide,
        };
        self.call(self.connection.client.claim(item, &request))
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
