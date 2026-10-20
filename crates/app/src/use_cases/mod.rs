//! The use cases, one per module, and the collection a composition root builds once.

mod ack;
mod acks;
mod add_dependency;
mod apply_settings;
mod authenticate;
mod claim_ending;
mod claim_item;
mod close_ui_session;
mod common;
mod complete;
mod create_item;
mod create_member;
mod edit_check;
mod follow_events;
mod force_release;
mod get_item;
mod grant;
mod grant_endings;
mod heartbeat;
mod inbox;
mod init_project;
mod list_completed;
mod list_events;
mod list_items;
mod list_members;
mod list_threads;
mod mark_read;
mod me;
mod open_ui_session;
mod operate_project;
mod post_message;
mod read_thread;
mod readable;
mod ready;
mod register_session;
mod release;
mod remove_dependency;
mod resolve_ui_session;
mod revoke_grant;
mod revoke_token;
mod rotate_token;
mod sent_status;
mod slots;
mod start_thread;
mod status;
mod take_over;
mod unacked;
mod upsert_items;
mod wide_asks;

#[cfg(test)]
pub(crate) mod testing;

pub use ack::{AckMessage, Acknowledged};
pub use add_dependency::{AddDependency, DependencyChange, DependencyInput};
pub use apply_settings::{AppliedSettings, ApplySettings};
pub use authenticate::Authenticate;
pub use claim_item::{ClaimItem, ClaimItemInput, ClaimMade, WideInput, WideNote};
pub use close_ui_session::CloseUiSession;
pub use common::NewMember;
pub use complete::Complete;
pub use create_item::{CreateItem, CreateItemInput, CreatedItem};
pub use create_member::{CreateMember, CreateMemberInput, CreatedMember};
pub use edit_check::{EditCheck, EditCheckInput, EditVerdicts};
pub use follow_events::{FollowEvents, FollowPage, FollowedEvent};
pub use force_release::ForceRelease;
pub use get_item::{GetItem, ItemView};
pub use grant::{Grant, GrantInput, GrantResult};
pub use heartbeat::{Heartbeat, HeartbeatOutcome};
pub use inbox::{Inbox, InboxQuery, InboxView, PendingOffer};
pub use init_project::{InitProject, InitProjectInput, InitializedProject};
pub use list_completed::{CompletedItem, ListCompleted, MAX_COMPLETED};
pub use list_events::{EventCursor, EventPage, ListEvents, MAX_PAGE};
pub use list_items::{ItemQuery, ListItems};
pub use list_members::ListMembers;
pub use list_threads::ListThreads;
pub use mark_read::MarkRead;
pub use me::{Me, MeView};
pub use open_ui_session::{OpenUiSession, OpenedUiSession, UI_SESSION_LIFETIME};
pub use operate_project::OperateProject;
pub use post_message::{PostMessage, PostMessageInput};
pub use read_thread::{ReadThread, RecipientAck, ThreadView};
pub use ready::{ExcludedEntry, NamedNotReady, Ready, ReadyEntry, ReadyView};
pub use register_session::{RegisterSession, RegisterSessionInput};
pub use release::Release;
pub use remove_dependency::RemoveDependency;
pub use resolve_ui_session::ResolveUiSession;
pub use revoke_grant::RevokeGrant;
pub use revoke_token::RevokeToken;
pub use rotate_token::{RotateToken, RotatedToken};
pub use sent_status::{RecipientStatus, SentStatus, SentStatusView};
pub use slots::{
    EndedHold, ForceReleaseSlot, ForceReleaseSlotInput, ForcedHold, HoldMode, LeaveSlotQueue,
    NamedHold, NamedOffer, NamedWaiter, ReleaseSlot, ReleaseSlotInput, RunFinishedInput, SlotLine,
    SlotRunBeat, SlotRunFinished, Slots, SweepReport, TakeOverSlot, TakeOverSlotInput, TakeSlot,
    TakeSlotInput, TakenOverSlot, TickSlots, TookKind, TookSlot,
};
pub use start_thread::{Addressees, StartThread, StartThreadInput, StartedThread};
pub use status::{AreaHold, HeldItem, Status, StatusView, WideAreaStanding, WideClaimStatus};
pub use take_over::{TakeOver, TakeOverInput, TakenOverClaim};
pub use unacked::{Unacked, UnackedMessage};
pub use upsert_items::{UpsertItems, UpsertReport};
pub use wide_asks::Announced;

use crate::dependencies::Dependencies;

/// Every use case, built once from the same dependencies.
pub struct UseCases {
    /// See [`Authenticate`].
    pub authenticate: Authenticate,
    /// See [`Me`].
    pub me: Me,
    /// See [`ListMembers`].
    pub list_members: ListMembers,
    /// See [`RegisterSession`].
    pub register_session: RegisterSession,
    /// See [`Heartbeat`].
    pub heartbeat: Heartbeat,
    /// See [`CreateItem`].
    pub create_item: CreateItem,
    /// See [`ListItems`].
    pub list_items: ListItems,
    /// See [`ListCompleted`].
    pub list_completed: ListCompleted,
    /// See [`GetItem`].
    pub get_item: GetItem,
    /// See [`UpsertItems`].
    pub upsert_items: UpsertItems,
    /// See [`Ready`].
    pub ready: Ready,
    /// See [`AddDependency`].
    pub add_dependency: AddDependency,
    /// See [`RemoveDependency`].
    pub remove_dependency: RemoveDependency,
    /// See [`ClaimItem`].
    pub claim_item: ClaimItem,
    /// See [`TakeOver`].
    pub take_over: TakeOver,
    /// See [`Release`].
    pub release: Release,
    /// See [`Complete`].
    pub complete: Complete,
    /// See [`Status`].
    pub status: Status,
    /// See [`EditCheck`].
    pub edit_check: EditCheck,
    /// See [`Grant`].
    pub grant: Grant,
    /// See [`RevokeGrant`].
    pub revoke_grant: RevokeGrant,
    /// See [`StartThread`].
    pub start_thread: StartThread,
    /// See [`PostMessage`].
    pub post_message: PostMessage,
    /// See [`Inbox`].
    pub inbox: Inbox,
    /// See [`ListThreads`].
    pub list_threads: ListThreads,
    /// See [`ReadThread`].
    pub read_thread: ReadThread,
    /// See [`MarkRead`].
    pub mark_read: MarkRead,
    /// See [`AckMessage`].
    pub ack: AckMessage,
    /// See [`Unacked`].
    pub unacked: Unacked,
    /// See [`SentStatus`].
    pub sent_status: SentStatus,
    /// See [`ListEvents`].
    pub list_events: ListEvents,
    /// See [`FollowEvents`].
    pub follow_events: FollowEvents,
    /// See [`InitProject`].
    pub init_project: InitProject,
    /// See [`OperateProject`].
    pub operate_project: OperateProject,
    /// See [`CreateMember`].
    pub create_member: CreateMember,
    /// See [`RotateToken`].
    pub rotate_token: RotateToken,
    /// See [`RevokeToken`].
    pub revoke_token: RevokeToken,
    /// See [`ApplySettings`].
    pub apply_settings: ApplySettings,
    /// See [`ForceRelease`].
    pub force_release: ForceRelease,
    /// See [`OpenUiSession`].
    pub open_ui_session: OpenUiSession,
    /// See [`ResolveUiSession`].
    pub resolve_ui_session: ResolveUiSession,
    /// See [`CloseUiSession`].
    pub close_ui_session: CloseUiSession,
    /// See [`TakeSlot`].
    pub take_slot: TakeSlot,
    /// See [`ReleaseSlot`].
    pub release_slot: ReleaseSlot,
    /// See [`LeaveSlotQueue`].
    pub leave_slot_queue: LeaveSlotQueue,
    /// See [`ForceReleaseSlot`].
    pub force_release_slot: ForceReleaseSlot,
    /// See [`SlotRunBeat`].
    pub slot_run_beat: SlotRunBeat,
    /// See [`TakeOverSlot`].
    pub take_over_slot: TakeOverSlot,
    /// See [`SlotRunFinished`].
    pub slot_run_finished: SlotRunFinished,
    /// See [`Slots`].
    pub slots: Slots,
    /// See [`TickSlots`].
    pub tick_slots: TickSlots,
}

impl UseCases {
    /// Builds every use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            authenticate: Authenticate::new(deps),
            me: Me::new(deps),
            list_members: ListMembers::new(deps),
            register_session: RegisterSession::new(deps),
            heartbeat: Heartbeat::new(deps),
            create_item: CreateItem::new(deps),
            list_items: ListItems::new(deps),
            list_completed: ListCompleted::new(deps),
            get_item: GetItem::new(deps),
            upsert_items: UpsertItems::new(deps),
            ready: Ready::new(deps),
            add_dependency: AddDependency::new(deps),
            remove_dependency: RemoveDependency::new(deps),
            claim_item: ClaimItem::new(deps),
            take_over: TakeOver::new(deps),
            release: Release::new(deps),
            complete: Complete::new(deps),
            status: Status::new(deps),
            edit_check: EditCheck::new(deps),
            grant: Grant::new(deps),
            revoke_grant: RevokeGrant::new(deps),
            start_thread: StartThread::new(deps),
            post_message: PostMessage::new(deps),
            inbox: Inbox::new(deps),
            list_threads: ListThreads::new(deps),
            read_thread: ReadThread::new(deps),
            mark_read: MarkRead::new(deps),
            ack: AckMessage::new(deps),
            unacked: Unacked::new(deps),
            sent_status: SentStatus::new(deps),
            list_events: ListEvents::new(deps),
            follow_events: FollowEvents::new(deps),
            init_project: InitProject::new(deps),
            operate_project: OperateProject::new(deps),
            create_member: CreateMember::new(deps),
            rotate_token: RotateToken::new(deps),
            revoke_token: RevokeToken::new(deps),
            apply_settings: ApplySettings::new(deps),
            force_release: ForceRelease::new(deps),
            open_ui_session: OpenUiSession::new(deps),
            resolve_ui_session: ResolveUiSession::new(deps),
            close_ui_session: CloseUiSession::new(deps),
            take_slot: TakeSlot::new(deps),
            release_slot: ReleaseSlot::new(deps),
            leave_slot_queue: LeaveSlotQueue::new(deps),
            force_release_slot: ForceReleaseSlot::new(deps),
            slot_run_beat: SlotRunBeat::new(deps),
            take_over_slot: TakeOverSlot::new(deps),
            slot_run_finished: SlotRunFinished::new(deps),
            slots: Slots::new(deps),
            tick_slots: TickSlots::new(deps),
        }
    }
}

#[cfg(test)]
mod integration_refusals {
    use dibs_domain::ids::{GrantId, ItemId, MessageId};
    use dibs_domain::message::SessionLabel;
    use dibs_domain::paths::RepoPath;
    use dibs_domain::session::{AgentLabel, HostName};

    use super::testing::Env;
    use super::*;
    use crate::AppError;

    #[tokio::test]
    async fn an_integration_is_refused_what_only_a_person_does() {
        let env = Env::new().await;
        let app = UseCases::new(&env.deps);
        let bot = &env.importer;
        let item = env.item("gh:acme/widgets#1").await;
        let session = env.alice.session;
        let refusals: Vec<Result<(), AppError>> = vec![
            app.status.execute(bot).await.map(|_| ()),
            app.register_session
                .execute(
                    bot,
                    RegisterSessionInput {
                        host: HostName::parse("worker").unwrap(),
                        label: SessionLabel::parse("worker/mcp").unwrap(),
                        agent: AgentLabel::parse("bot").unwrap(),
                        push: false,
                    },
                )
                .await
                .map(|_| ()),
            app.heartbeat.execute(bot, session).await.map(|_| ()),
            app.edit_check
                .execute(
                    bot,
                    EditCheckInput {
                        session,
                        paths: vec![RepoPath::parse("default:packages/billing/a.rs").unwrap()],
                        reserve: false,
                    },
                )
                .await
                .map(|_| ()),
            app.release.execute(bot, item).await.map(|_| ()),
            app.complete.execute(bot, ItemId(9_999)).await.map(|_| ()),
            app.revoke_grant
                .execute(bot, GrantId(9_999))
                .await
                .map(|_| ()),
            app.add_dependency
                .execute(
                    bot,
                    DependencyInput {
                        item,
                        blocked_by: ItemId(9_999),
                    },
                )
                .await
                .map(|_| ()),
            app.remove_dependency
                .execute(
                    bot,
                    DependencyInput {
                        item,
                        blocked_by: ItemId(9_999),
                    },
                )
                .await
                .map(|_| ()),
            app.ack.execute(bot, MessageId(9_999)).await.map(|_| ()),
            app.unacked.execute(bot).await.map(|_| ()),
            app.sent_status
                .execute(bot, MessageId(9_999))
                .await
                .map(|_| ()),
        ];
        for (n, refusal) in refusals.into_iter().enumerate() {
            assert_eq!(refusal.unwrap_err().code(), "forbidden", "case {n}");
        }
        assert!(app.me.execute(bot).await.is_ok());
        assert!(app.list_members.execute(bot).await.is_ok());
        assert!(
            app.list_items
                .execute(bot, ItemQuery::default())
                .await
                .is_ok()
        );
        assert!(app.get_item.execute(bot, item).await.is_ok());
        assert!(app.ready.execute(bot).await.is_ok());
    }
}
