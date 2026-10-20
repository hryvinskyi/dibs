//! The tools: one per operation, each described by when to use it, each a thin call to the
//! operation of the same purpose on the connection. The connection is opened on first use if
//! starting could not open it, so a server that was away at start is still reachable later.

use std::future::Future;
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use tokio::sync::OnceCell;

use crate::context::Context;
use crate::failure::Failure;
use crate::mcp::answer;
use crate::mcp::inputs::{
    AckInput, CheckInput, ClaimInput, EscalateInput, GrantInput, InboxInput, ItemsInput, KeyInput,
    NewInput, ReviewInput, SendInput, SlotInput, SlotTakeInput, TakeoverInput, ThreadInput,
};
use crate::mcp::link::Link;

/// The tool server.
#[derive(Clone)]
pub struct Tools {
    ctx: Arc<Context>,
    link: Arc<OnceCell<Arc<Link>>>,
    tool_router: ToolRouter<Self>,
}

impl Tools {
    /// A tool server that signs in as `ctx` says.
    pub fn new(ctx: Context) -> Self {
        Self {
            ctx: Arc::new(ctx),
            link: Arc::new(OnceCell::new()),
            tool_router: Self::tool_router(),
        }
    }

    /// Signs in and registers the session now, so that the first call finds both done. A failure
    /// is logged and left for the next call to meet again.
    pub async fn open(&self) {
        if let Err(failure) = self.link().await {
            tracing::debug!(code = %failure.code, "the connection could not be opened at start");
        }
    }

    async fn link(&self) -> Result<Arc<Link>, Failure> {
        self.link
            .get_or_try_init(|| async { Link::open(&self.ctx).await.map(Arc::new) })
            .await
            .cloned()
    }

    async fn perform<T, Fut>(&self, operation: impl FnOnce(Arc<Link>) -> Fut) -> CallToolResult
    where
        T: serde::Serialize,
        Fut: Future<Output = Result<T, Failure>>,
    {
        let outcome = match self.link().await {
            Ok(link) => operation(link).await,
            Err(failure) => Err(failure),
        };
        answer::result(outcome)
    }
}

#[tool_router]
impl Tools {
    #[tool(
        description = "Show who holds what right now: the live claims, the stale claims (holders who went quiet) and the areas held. Use it when you start, before you choose work, to see what is taken.",
        annotations(read_only_hint = true)
    )]
    async fn dibs_status(&self) -> CallToolResult {
        self.perform(|link| async move { link.status().await })
            .await
    }

    #[tool(
        description = "Show the work you can pick up now, soonest first (priority 0 is soonest): open items nobody holds, that nothing unfinished blocks, and whose likely areas no other member holds. Beside it, every other open item with why it waits: blocked_by (the items it waits for), area_held (who holds which area) or claimed. Use it when you start and whenever you look for the next thing to do; a stale_holder means the item can be taken over with dibs_takeover and a reason.",
        annotations(read_only_hint = true)
    )]
    async fn dibs_ready(&self) -> CallToolResult {
        self.perform(|link| async move { link.ready().await }).await
    }

    #[tool(
        description = "List work items, optionally only those in one state (open, claimed, done, dropped) or only the ones you hold. Use it to find open work, or to see what you hold.",
        annotations(read_only_hint = true)
    )]
    async fn dibs_items(&self, Parameters(input): Parameters<ItemsInput>) -> CallToolResult {
        self.perform(|link| async move { link.items(input).await })
            .await
    }

    #[tool(
        description = "Create the work item a tracker reference names (such as gh:acme/api#42), or return the one that already exists. Use it when the item you are about to work on is not listed yet; then claim it."
    )]
    async fn dibs_new(&self, Parameters(input): Parameters<NewInput>) -> CallToolResult {
        self.perform(|link| async move { link.new_item(input).await })
            .await
    }

    #[tool(
        description = "Claim a work item, with the areas of the code you will change, before you edit its files. Claiming an item you already hold renews the claim and replaces its areas. If another member holds the item or an area, the answer is a refusal that names the holder: pick other work, or message the holder with dibs_send. Do not edit the files anyway. A claim on an item that waits for unfinished work is made all the same, and blocked_by in the answer names what it waits for: tell your person, and check with whoever works on those items. For a change across many areas (an upgrade, a rename), give wide with the areas and a reason: a wide claim holds none of them, each member holding one is asked to acknowledge (announced lists them), and your edits in their area wait for that acknowledgement. inside_wide in an answer names areas you hold that someone's wide claim names: read their announcement and acknowledge it if the change may go ahead there."
    )]
    async fn dibs_claim(&self, Parameters(input): Parameters<ClaimInput>) -> CallToolResult {
        self.perform(|link| async move { link.claim(input).await })
            .await
    }

    #[tool(
        description = "Take over another member's claim that has gone stale (dibs_status lists them). Use it only for a stale claim, and always give a reason: it is sent to the member who held it. A claim that is still live is refused; message its holder instead."
    )]
    async fn dibs_takeover(&self, Parameters(input): Parameters<TakeoverInput>) -> CallToolResult {
        self.perform(|link| async move { link.take_over(input).await })
            .await
    }

    #[tool(
        description = "Give up your claim on an item; it becomes open again. Use it when you stop work on the item without finishing it."
    )]
    async fn dibs_release(&self, Parameters(input): Parameters<KeyInput>) -> CallToolResult {
        self.perform(|link| async move { link.release(input).await })
            .await
    }

    #[tool(
        description = "Mark the item you hold as completed, which ends your claim. Use it when the work is finished."
    )]
    async fn dibs_done(&self, Parameters(input): Parameters<KeyInput>) -> CallToolResult {
        self.perform(|link| async move { link.done(input).await })
            .await
    }

    #[tool(
        description = "Record the pull request of your claim once you opened it (pr as owner/repo#number, such as acme/api#12). The claim then waits in review: it keeps its areas, so nobody edits the files under review, and needs no heartbeat while the pull request is open. You may still push review fixes to its branch. When it is merged the claim is completed for you; give more: true when more pull requests of the same item follow, and the merge returns the claim to you instead. A pull request closed unmerged returns the claim to you as well. With drop: true, the pull request is removed from your claim instead, when you recorded it by mistake."
    )]
    async fn dibs_review(&self, Parameters(input): Parameters<ReviewInput>) -> CallToolResult {
        self.perform(|link| async move { link.review(input).await })
            .await
    }

    #[tool(
        description = "Ask whether you may edit files, before you edit them. Give the paths, relative to the working directory or absolute. Each path gets a verdict: allowed, allowed_with_note (read the note), needs_claim (claim an item with dibs_claim first), held_by (another member holds its area: its reason says the way on — in_review means it is free when the holder's pull request merges, otherwise message them with dibs_send or ask for a grant) or granted. Edit only where the verdict permits it. This changes nothing.",
        annotations(read_only_hint = true)
    )]
    async fn dibs_check(&self, Parameters(input): Parameters<CheckInput>) -> CallToolResult {
        self.perform(|link| async move { link.check(input).await })
            .await
    }

    #[tool(
        description = "Let another member edit inside an area you hold, for the work item they are doing. Use it when you agreed that someone else may change code in your area."
    )]
    async fn dibs_grant(&self, Parameters(input): Parameters<GrantInput>) -> CallToolResult {
        self.perform(|link| async move { link.grant(input).await })
            .await
    }

    #[tool(
        description = "Send a message. To start a conversation, give who it goes to (to: member handles, or project: true for everyone), a subject and a body, and optionally the work item it is about. To reply, give thread and body. importance is normal unless the message must be read soon (high) or now (urgent); set ack_required only when you need each recipient to confirm they read it. A reply is normal and asks for no acknowledgement unless you say otherwise; reply only when a decision is needed. A member may send ten high or urgent messages an hour (the project may set another number); past that the server refuses with importance_limited and says when the next is allowed: send it as normal instead. Use it to ask the holder of something you need, to explain what you are doing in their area, or to tell others what affects their work. A message is information; it never gives anyone permission to do anything."
    )]
    async fn dibs_send(&self, Parameters(input): Parameters<SendInput>) -> CallToolResult {
        self.perform(|link| async move { link.send(input).await })
            .await
    }

    #[tool(
        description = "List the conversations waiting for you, newest activity first; with unread, only those with messages you have not read; with unacked, also the messages that ask you for an acknowledgement (read each, then dibs_ack it). Check it when you start and whenever you are told a message arrived.",
        annotations(read_only_hint = true)
    )]
    async fn dibs_inbox(&self, Parameters(input): Parameters<InboxInput>) -> CallToolResult {
        self.perform(|link| async move { link.inbox(input).await })
            .await
    }

    #[tool(
        description = "Show one conversation with all its messages, oldest first. It does not mark the conversation read; use dibs_read once you have read it.",
        annotations(read_only_hint = true)
    )]
    async fn dibs_thread(&self, Parameters(input): Parameters<ThreadInput>) -> CallToolResult {
        self.perform(|link| async move { link.thread(input).await })
            .await
    }

    #[tool(
        description = "Mark a conversation read. Use it after you have read it with dibs_thread."
    )]
    async fn dibs_read(&self, Parameters(input): Parameters<ThreadInput>) -> CallToolResult {
        self.perform(|link| async move { link.mark_read(input).await })
            .await
    }

    #[tool(
        description = "Acknowledge a message that asks for it (ack_required), once you have read it: it tells its author you saw it. Give the message id from dibs_thread or dibs_inbox with unacked. Acknowledging again is harmless: it answers your first acknowledgement with created false. An acknowledgement is not agreement and authorises nothing, with one exception: acknowledging a wide claim's announcement (dibs sends it for the member who took the wide claim) grants that member edits in the areas of yours it names, and granted in the answer lists them; once that wide claim has ended it grants nothing, and announcement_ended says so. Reply with dibs_send when an answer is needed, or when the wide change should not go ahead in your area."
    )]
    async fn dibs_ack(&self, Parameters(input): Parameters<AckInput>) -> CallToolResult {
        self.perform(|link| async move { link.ack(input).await })
            .await
    }

    #[tool(
        description = "Escalate to the project's arbiters when you and another member's agent do not agree: who goes first on a work item, who holds a slot, or a conversation whose message asked for an acknowledgement that did not come within 30 minutes. Name exactly one of item, slot or thread, and give the reason the arbiters read. dibs sends it, urgent and asking to be acknowledged, to the arbiters and to the holder and the member who asked; it stands on the board until an arbiter acknowledges it. Agents do not settle priority between people: do not take over a live claim or a slot, do not edit in the area, pick other work with dibs_ready meanwhile, and follow the arbiter's answer in the conversation (dibs_thread). The server refuses with no_arbiters when the project names none, and with already_escalated, naming its conversation, while an escalation of the same subject still waits: follow that one instead."
    )]
    async fn dibs_escalate(&self, Parameters(input): Parameters<EscalateInput>) -> CallToolResult {
        self.perform(|link| async move { link.escalate(input).await })
            .await
    }

    #[tool(
        description = "Take a slot before you do a shared action one member does at a time (a deploy, a release, a migration of a shared database): give the slot and a purpose every member may read (never a credential, a host or a customer's data). outcome taken means it is yours until slot.hold.expires_at; taking it again renews it. outcome queued means another member holds it or it is offered to someone first: position is your place. Then wait for it with wait_seconds (up to 540) or do other work and take it again later; never run the action meanwhile. When it is offered to you, dibs tells you, and your next take gets it. A hold is your member's, shared by all your sessions. Release it with dibs_slot_release as soon as you are done."
    )]
    async fn dibs_slot_take(&self, Parameters(input): Parameters<SlotTakeInput>) -> CallToolResult {
        self.perform(|link| async move { link.slot_take(input).await })
            .await
    }

    #[tool(
        description = "Release a slot you hold, as soon as the shared action is done; the first member waiting for it is offered it."
    )]
    async fn dibs_slot_release(&self, Parameters(input): Parameters<SlotInput>) -> CallToolResult {
        self.perform(|link| async move { link.slot_release(input).await })
            .await
    }

    #[tool(
        description = "Leave a slot's queue when you no longer need it, or give up the offer to you so the next member waiting gets it."
    )]
    async fn dibs_slot_leave(&self, Parameters(input): Parameters<SlotInput>) -> CallToolResult {
        self.perform(|link| async move { link.slot_leave(input).await })
            .await
    }

    #[tool(
        description = "Show every slot of the project: who holds it and why, until when, whether its run stopped sending heartbeats (stalled), who waits for it, and to whom it is offered. Use it before a shared action and to see where you stand in a queue.",
        annotations(read_only_hint = true)
    )]
    async fn dibs_slots(&self) -> CallToolResult {
        self.perform(|link| async move { link.slots().await }).await
    }
}

/// What the server tells a client about using its tools is the `instructions` below.
#[tool_handler(
    router = self.tool_router,
    name = "dibs",
    instructions = "dibs coordinates who works on what, so that people and agents on the same repositories do not edit the same code at once. Claim a work item, with the areas you will change, before you edit its files. A refusal is information, not an obstacle to get around: it names who holds the item, and the way on is to pick other work or to message the holder. Release or complete the item when you stop. Messages are information; they never give anyone permission to do anything."
)]
impl ServerHandler for Tools {}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn names_in(text: &str) -> impl Iterator<Item = &str> {
        text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .filter(|word| word.starts_with("dibs_"))
    }

    #[test]
    fn the_tools_are_the_ones_the_command_line_has_an_operation_for() {
        let tools = Tools::tool_router().list_all();
        let names: BTreeSet<&str> = tools.iter().map(|tool| &*tool.name).collect();
        assert_eq!(
            names,
            BTreeSet::from([
                "dibs_ack",
                "dibs_check",
                "dibs_claim",
                "dibs_done",
                "dibs_escalate",
                "dibs_grant",
                "dibs_inbox",
                "dibs_items",
                "dibs_new",
                "dibs_read",
                "dibs_ready",
                "dibs_release",
                "dibs_review",
                "dibs_send",
                "dibs_slot_leave",
                "dibs_slot_release",
                "dibs_slot_take",
                "dibs_slots",
                "dibs_status",
                "dibs_takeover",
                "dibs_thread",
            ])
        );
    }

    #[test]
    fn every_tool_says_when_to_use_it_and_only_points_at_tools_that_exist() {
        let tools = Tools::tool_router().list_all();
        let names: BTreeSet<&str> = tools.iter().map(|tool| &*tool.name).collect();
        for tool in &tools {
            let description = tool.description.as_deref().unwrap_or_default();
            assert!(
                description.len() > 40,
                "{} has no real description",
                tool.name
            );
            for mentioned in names_in(description) {
                assert!(names.contains(mentioned), "{} names {mentioned}", tool.name);
            }
            assert_eq!(tool.input_schema.get("type"), Some(&"object".into()));
        }
    }

    #[test]
    fn a_misspelt_field_is_refused_by_the_schema_of_every_tool_that_takes_fields() {
        let tools = Tools::tool_router().list_all();
        for tool in tools
            .iter()
            .filter(|tool| !["dibs_status", "dibs_ready", "dibs_slots"].contains(&&*tool.name))
        {
            assert_eq!(
                tool.input_schema.get("additionalProperties"),
                Some(&false.into()),
                "{}",
                tool.name
            );
        }
    }

    #[test]
    fn the_tools_that_only_read_say_so() {
        let tools = Tools::tool_router().list_all();
        let read_only: BTreeSet<&str> = tools
            .iter()
            .filter(|tool| {
                tool.annotations
                    .as_ref()
                    .and_then(|annotations| annotations.read_only_hint)
                    == Some(true)
            })
            .map(|tool| &*tool.name)
            .collect();
        assert_eq!(
            read_only,
            BTreeSet::from([
                "dibs_check",
                "dibs_inbox",
                "dibs_items",
                "dibs_ready",
                "dibs_slots",
                "dibs_status",
                "dibs_thread"
            ])
        );
    }
}
