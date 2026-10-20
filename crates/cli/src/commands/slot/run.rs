//! `dibs slot run`: take a slot, run a command while holding it, and release it when the command
//! ends.
//!
//! The hold is kept by this run's heartbeats; without them for two minutes it is stalled, not
//! released, so nobody acts on a half-done deploy as if it were free. An interruption the command
//! does not get on its own (`SIGTERM`, `SIGHUP`) is passed to it, and the hold is released, once
//! the command ended, with the interruption as the exit. A Ctrl-C reaches the command from the
//! terminal; the wrapper waits for it and releases with the command's own outcome. When a heartbeat learns that the hold is no longer this run's — taken
//! over or ended meanwhile — the wrapper says so once, lets the command run on, and releases
//! nothing when it ends.

use std::process::ExitStatus;

use tokio::process::Command;
use tokio::time::MissedTickBehavior;

use dibs_client::api::{HoldMode, ReleaseSlotRequest, TakeOutcome};
use dibs_domain::ids::RunId;
use dibs_domain::slot::{EndWord, RunExit, SlotName};

use crate::cli::SlotRunArgs;
use crate::commands::slot::platform::{self, Interrupt, Interrupts};
use crate::commands::slot::{request, take_waiting, took_text};
use crate::context::{Connection, Context};
use crate::failure::Failure;

/// Runs the command under the slot.
///
/// # Errors
/// The [`Failure`] of the take; a refusal when the caller is still queued; the command's own exit
/// status when it does not succeed.
pub async fn run_command(args: SlotRunArgs, ctx: &Context) -> Result<(), Failure> {
    let connection = ctx.connect().await?;
    let request = request(&connection, &args.ask, HoldMode::Run, None).await?;
    let slot = args.ask.slot.clone();
    let took = take_waiting(
        &connection.client,
        &slot,
        &request,
        args.ask.wait,
        args.ask.poll_every,
    )
    .await
    .map_err(|error| connection.failed(error))?;
    let me = &connection.me.member.handle;
    let (TakeOutcome::Taken | TakeOutcome::Renewed, Some(run)) = (took.outcome, took.run_id) else {
        ctx.printer.note(&took_text(&took, me));
        return Err(Failure::refused(
            "queued",
            format!("{slot} is not yours yet, so the command did not run."),
        ));
    };
    ctx.printer.note(&format!(
        "{} The command runs now; the slot is released when it ends.",
        took_text(&took, me)
    ));
    let Some((program, rest)) = args.command.split_first() else {
        return Err(Failure::config(
            "no_command",
            "Give the command to run after --.",
        ));
    };
    let mut child = match Command::new(program).args(rest).spawn() {
        Ok(child) => child,
        Err(error) => {
            tracing::debug!(error = %error, "the command could not be started");
            release(&connection, ctx, &slot, run, None).await;
            return Err(Failure::config(
                "command_not_started",
                format!(
                    "The command {} could not be started; {slot} was released.",
                    program.to_string_lossy()
                ),
            ));
        }
    };
    let mut interrupts = Interrupts::listen().map_err(|_| {
        Failure::config(
            "no_signals",
            "This system does not let dibs listen for interruptions, so the command did not run.",
        )
    })?;
    let mut beats = tokio::time::interval(args.heartbeat_every);
    beats.set_missed_tick_behavior(MissedTickBehavior::Delay);
    beats.tick().await;
    let mut ours = true;
    let mut unreachable_said = false;
    let mut interrupted: Option<Interrupt> = None;
    let status = loop {
        tokio::select! {
            status = child.wait() => break status,
            interrupt = interrupts.next() => {
                platform::forward(interrupt, child.id());
                if interrupt.passed_on() {
                    interrupted.get_or_insert(interrupt);
                }
            }
            _ = beats.tick(), if ours => {
                match connection.client.slot_heartbeat(&slot, run).await {
                    Ok(_) => {}
                    Err(error) if matches!(error.code(), "taken_over" | "run_ended") => {
                        ours = false;
                        let who = error.details().and_then(|details| details.holder.clone());
                        ctx.printer.note(&match who {
                            Some(holder) => format!("{holder} took {slot} over while this command ran. It keeps running, and nothing is released when it ends."),
                            None => format!("The hold on {slot} ended while this command ran. It keeps running, and nothing is released when it ends."),
                        });
                    }
                    Err(error) => {
                        tracing::debug!(error = %error, "the heartbeat did not reach the server");
                        if !unreachable_said {
                            unreachable_said = true;
                            ctx.printer.note(&format!("The heartbeat of {slot} did not reach the server; the hold is shown as stalled after two minutes without one. The command keeps running."));
                        }
                    }
                }
            }
        }
    };
    let status = status.map_err(|_| {
        Failure::config(
            "command_lost",
            "dibs lost track of the command it started; release the slot with dibs slot release once it has ended.",
        )
    })?;
    let exit = exit_of(&status, interrupted);
    if ours {
        release(&connection, ctx, &slot, run, exit.clone()).await;
    }
    finish(&status, exit.as_ref())
}

/// How the command ended, as the release records it: the interruption passed to it, the signal
/// that ended it, or its status.
fn exit_of(status: &ExitStatus, interrupted: Option<Interrupt>) -> Option<RunExit> {
    let signal = interrupted
        .map(Interrupt::name)
        .or_else(|| platform::ended_by(status));
    match signal {
        Some(name) => EndWord::parse(name).ok().map(RunExit::Signal),
        None => status.code().map(RunExit::Code),
    }
}

/// The wrapper's own exit: the command's status, or the shell's status for a signal that ended
/// it.
fn finish(status: &ExitStatus, exit: Option<&RunExit>) -> Result<(), Failure> {
    let code = match (status.code(), exit) {
        (Some(code), _) => u8::try_from(code).unwrap_or(u8::MAX),
        (None, Some(RunExit::Signal(name))) => platform::status_for_name(name.as_str()),
        (None, _) => u8::MAX,
    };
    if code == 0 {
        return Ok(());
    }
    let how = match exit {
        Some(RunExit::Signal(name)) => format!("on {name}"),
        Some(RunExit::Code(code)) => format!("with status {code}"),
        Some(RunExit::Conclusion(word)) => format!("as {word}"),
        None => "abnormally".to_owned(),
    };
    Err(Failure::passed(code, format!("The command ended {how}.")))
}

/// Releases the run's hold with its exit; a hold no longer the run's is left alone and said so.
async fn release(
    connection: &Connection,
    ctx: &Context,
    slot: &SlotName,
    run: RunId,
    exit: Option<RunExit>,
) {
    let request = ReleaseSlotRequest {
        run_id: Some(run),
        exit,
    };
    match connection.client.release_slot(slot, &request).await {
        Ok(released) => {
            let mut text = format!("Released {slot}.");
            if let Some(offer) = &released.slot.offer {
                text.push_str(&format!(" It is offered to {} now.", offer.member));
            }
            ctx.printer.note(&text);
        }
        Err(error) if matches!(error.code(), "run_ended" | "not_holder" | "slot_not_held") => {
            ctx.printer.note(&format!(
                "The hold on {slot} was no longer this run's, so nothing was released."
            ));
        }
        Err(error) => {
            tracing::debug!(error = %error, "the release did not reach the server");
            ctx.printer.note(&format!(
                "{slot} could not be released: the server did not answer. Release it with dibs slot release {slot}."
            ));
        }
    }
}
