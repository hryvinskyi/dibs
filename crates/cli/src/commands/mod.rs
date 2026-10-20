//! The commands. Each one reads its arguments, makes the calls it needs through the client, and
//! prints the answer; none decides what the server decides.

pub mod admin;
pub mod check;
pub mod doctor;
pub mod graph;
pub mod hook;
pub mod login;
pub mod messages;
pub mod self_update;
pub mod slot;
pub mod sync;
pub mod work;

use crate::cli::Command;
use crate::context::Context;
use crate::failure::Failure;

/// Runs a command.
///
/// # Errors
/// The command's [`Failure`].
pub async fn run(command: Command, ctx: &Context) -> Result<(), Failure> {
    match command {
        Command::Login(args) => login::login(args, ctx).await,
        Command::Whoami => login::whoami(ctx).await,
        Command::Status => work::status(ctx).await,
        Command::Items(args) => work::items(args, ctx).await,
        Command::Ready => graph::ready(ctx).await,
        Command::Dep(command) => graph::dep(command, ctx).await,
        Command::New(args) => work::new(args, ctx).await,
        Command::Claim(args) => work::claim(args, ctx).await,
        Command::Takeover(args) => work::take_over(args, ctx).await,
        Command::Release(args) => work::release(args, ctx).await,
        Command::Done(args) => work::done(args, ctx).await,
        Command::Grant(args) => work::grant(args, ctx).await,
        Command::Check(args) => check::check(args, ctx).await,
        Command::Send(args) => messages::send(args, ctx).await,
        Command::Inbox(args) => messages::inbox(args, ctx).await,
        Command::Ack(args) => messages::ack(args, ctx).await,
        Command::Escalate(args) => messages::escalate(args, ctx).await,
        Command::Thread(args) => messages::thread(args, ctx).await,
        Command::Read(args) => messages::read(args, ctx).await,
        Command::Slot(command) => slot::run(command, ctx).await,
        Command::Sync(args) => sync::sync(args, ctx).await,
        Command::Mcp => crate::mcp::serve(ctx).await,
        Command::SelfUpdate(args) => self_update::self_update(args, ctx).await,
        Command::Admin(command) => admin::run(command, ctx).await,
    }
}
