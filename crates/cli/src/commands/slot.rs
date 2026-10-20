//! Slots: `slot take`, `slot run`, `slot release`, `slot leave`, `slot list`, `slot test`,
//! `slot takeover`, and the admin's `admin slot force-release`.

mod platform;
mod run;

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use dibs_client::api::{
    ForceReleaseSlotRequest, HoldMode, ReleaseSlotRequest, Slot, SlotHold, TakeOutcome,
    TakeOverSlotRequest, TakeSlotRequest, Took,
};
use dibs_client::{Client, ClientError};
use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::MemberId;
use dibs_domain::settings::{ProjectSettings, SettingsDocument};
use dibs_domain::slot::{
    CommandVerdict, CommitSha, SlotBoard, SlotName, WorkflowName, command_verdict, push_targets,
    split_commands,
};

use crate::cli::{
    AdminSlotCommand, SlotAsk, SlotCommand, SlotTakeArgs, SlotTakeoverArgs, SlotTestArgs,
};
use crate::context::{Connection, Context};
use crate::failure::{Failure, sentence};

pub use run::run_command;

/// Runs a slot command.
///
/// # Errors
/// The command's [`Failure`].
pub async fn run(command: SlotCommand, ctx: &Context) -> Result<(), Failure> {
    match command {
        SlotCommand::Take(args) => take(args, ctx).await,
        SlotCommand::Run(args) => run_command(args, ctx).await,
        SlotCommand::Release(args) => release(&args.slot, ctx).await,
        SlotCommand::Leave(args) => leave(&args.slot, ctx).await,
        SlotCommand::List => list(ctx).await,
        SlotCommand::Test(args) => test(&args, ctx),
        SlotCommand::Takeover(args) => take_over(args, ctx).await,
    }
}

/// Runs an admin's slot command.
///
/// # Errors
/// The command's [`Failure`].
pub async fn admin(
    command: AdminSlotCommand,
    connection: &Connection,
    ctx: &Context,
) -> Result<(), Failure> {
    match command {
        AdminSlotCommand::ForceRelease(args) => {
            let session = connection.session().await?;
            let forced = connection
                .client
                .force_release_slot(
                    &args.slot,
                    &ForceReleaseSlotRequest {
                        session_id: session,
                        reason: args.reason,
                    },
                )
                .await
                .map_err(|error| connection.failed(error))?;
            ctx.printer.show(&forced, || {
                format!(
                    "Ended {}'s hold on {}; they were told why.",
                    forced.holder, args.slot
                )
            });
            Ok(())
        }
    }
}

/// Takes the slot — renewing the caller's hold or queueing them — and, with a wait, asks again
/// every `poll` until it is the caller's or the wait is over. Each ask keeps the caller's place
/// alive, and the slot offered to them is theirs on the next ask.
///
/// # Errors
/// The [`ClientError`] of a call.
pub async fn take_waiting(
    client: &Client,
    slot: &SlotName,
    request: &TakeSlotRequest,
    wait: Option<Duration>,
    poll: Duration,
) -> Result<Took, ClientError> {
    let deadline = wait.map(|wait| Instant::now() + wait);
    loop {
        let took = client.take_slot(slot, request).await?;
        if took.outcome != TakeOutcome::Queued {
            return Ok(took);
        }
        let Some(left) =
            deadline.and_then(|deadline| deadline.checked_duration_since(Instant::now()))
        else {
            return Ok(took);
        };
        if left.is_zero() {
            return Ok(took);
        }
        tokio::time::sleep(left.min(poll)).await;
    }
}

/// The take a command line asks for, its item looked up.
async fn request(
    connection: &Connection,
    ask: &SlotAsk,
    mode: HoldMode,
    until: Option<(WorkflowName, CommitSha)>,
) -> Result<TakeSlotRequest, Failure> {
    let item = match &ask.item {
        Some(key) => Some(connection.resolve(key).await?),
        None => None,
    };
    let (workflow, sha) = until.map_or((None, None), |(workflow, sha)| (Some(workflow), Some(sha)));
    Ok(TakeSlotRequest {
        session_id: connection.session().await?,
        purpose: ask.purpose.clone(),
        item,
        mode,
        workflow,
        sha,
    })
}

/// `workflow@sha`.
fn until_run(text: &str) -> Result<(WorkflowName, CommitSha), Failure> {
    let invalid = || {
        Failure::config(
            "invalid_until_run",
            "--until-run takes a workflow and a commit, written 'deploy.yml@<sha>'.",
        )
    };
    let (workflow, sha) = text.rsplit_once('@').ok_or_else(invalid)?;
    Ok((
        WorkflowName::parse(workflow).map_err(|_| invalid())?,
        CommitSha::parse(sha).map_err(|_| invalid())?,
    ))
}

async fn take(args: SlotTakeArgs, ctx: &Context) -> Result<(), Failure> {
    let until = args.until_run.as_deref().map(until_run).transpose()?;
    let mode = if until.is_some() {
        HoldMode::UntilRun
    } else {
        HoldMode::Session
    };
    let connection = ctx.connect().await?;
    let request = request(&connection, &args.ask, mode, until).await?;
    let took = take_waiting(
        &connection.client,
        &args.ask.slot,
        &request,
        args.ask.wait,
        args.ask.poll_every,
    )
    .await
    .map_err(|error| connection.failed(error))?;
    let me = &connection.me.member.handle;
    ctx.printer.show(&took, || took_text(&took, me));
    if took.outcome == TakeOutcome::Queued {
        return Err(Failure::refused(
            "queued",
            format!(
                "You are still waiting for {}; it is not yours yet.",
                args.ask.slot
            ),
        )
        .after_answer());
    }
    Ok(())
}

async fn release(slot: &SlotName, ctx: &Context) -> Result<(), Failure> {
    let connection = ctx.connect().await?;
    let released = connection
        .client
        .release_slot(slot, &ReleaseSlotRequest::default())
        .await
        .map_err(|error| connection.failed(error))?;
    ctx.printer.show(&released, || {
        let mut text = format!("Released {slot}.");
        if let Some(offer) = &released.slot.offer {
            let _ = write!(
                text,
                " It is offered to {} until {}.",
                offer.member,
                clock(offer.until)
            );
        }
        text
    });
    Ok(())
}

async fn leave(slot: &SlotName, ctx: &Context) -> Result<(), Failure> {
    let connection = ctx.connect().await?;
    let left = connection
        .client
        .leave_slot(slot)
        .await
        .map_err(|error| connection.failed(error))?;
    ctx.printer
        .show(&left, || format!("You no longer wait for {slot}."));
    Ok(())
}

async fn list(ctx: &Context) -> Result<(), Failure> {
    let connection = ctx.connect().await?;
    let slots = connection
        .client
        .slots()
        .await
        .map_err(|error| connection.failed(error))?;
    ctx.printer.show(&slots, || {
        if slots.slots.is_empty() {
            return "The project declares no slots.".to_owned();
        }
        slots
            .slots
            .iter()
            .map(|slot| {
                format!(
                    "{}: {}",
                    slot.name,
                    standing(slot, &connection.me.member.handle)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    });
    Ok(())
}

async fn take_over(args: SlotTakeoverArgs, ctx: &Context) -> Result<(), Failure> {
    let connection = ctx.connect().await?;
    let session = connection.session().await?;
    let taken = connection
        .client
        .take_over_slot(
            &args.slot,
            &TakeOverSlotRequest {
                session_id: session,
                purpose: args.purpose,
                reason: args.reason,
            },
        )
        .await
        .map_err(|error| connection.failed(error))?;
    ctx.printer.show(&taken, || {
        format!(
            "You hold {} now; {} was told what you checked.",
            args.slot, taken.previous.holder
        )
    });
    Ok(())
}

/// What `dibs slot test` prints: the simple commands of the line and its verdict as if every slot
/// were free, and which held slot would freeze a push it makes.
#[derive(Debug, serde::Serialize)]
struct TestAnswer {
    commands: Vec<String>,
    verdict: &'static str,
    slot: Option<SlotName>,
    run_only: bool,
    frozen_while_held: Vec<SlotName>,
}

fn test(args: &SlotTestArgs, ctx: &Context) -> Result<(), Failure> {
    let path = if args.settings.is_absolute() {
        args.settings.clone()
    } else {
        ctx.cwd.join(&args.settings)
    };
    let text = std::fs::read_to_string(&path).map_err(|_| {
        Failure::config(
            "unreadable_settings",
            format!(
                "The settings file {} cannot be read.",
                args.settings.display()
            ),
        )
    })?;
    let document: SettingsDocument = toml::from_str(&text).map_err(|error| {
        Failure::config(
            "invalid_settings",
            sentence(&format!(
                "the settings file is not valid: {}",
                error.message()
            )),
        )
    })?;
    let settings = ProjectSettings::parse(&document, &HotRuleRegistry::standard())
        .map_err(|error| Failure::config("invalid_settings", sentence(&error.to_string())))?;
    let repo = settings
        .checkout_repo(args.repo.as_ref())
        .map_err(|error| Failure::config("unknown_repo", sentence(&error.to_string())))?
        .id
        .clone();
    let commands = split_commands(&args.line.join(" "));
    let verdict = command_verdict(
        &commands,
        &repo,
        settings.slots(),
        &SlotBoard::default(),
        MemberId(0),
        None,
    );
    let frozen_while_held: Vec<SlotName> = commands
        .iter()
        .filter_map(|command| push_targets(command, &repo))
        .flatten()
        .flat_map(|target| {
            settings
                .slots()
                .slots()
                .iter()
                .filter(move |spec| spec.freezes.contains(&target))
                .map(|spec| spec.name.clone())
        })
        .collect();
    let (name, slot, run_only) = match verdict {
        CommandVerdict::Free => ("free", None, false),
        CommandVerdict::NeedsSlot { slot, run_only, .. } => ("needs_slot", Some(slot), run_only),
        CommandVerdict::Holding { slot } => ("holding", Some(slot), false),
        CommandVerdict::Frozen { slot, .. } => ("frozen", Some(slot), false),
        CommandVerdict::NeedsPermission { .. } => ("needs_permission", None, false),
    };
    let answer = TestAnswer {
        commands: commands.into_iter().map(|command| command.text).collect(),
        verdict: name,
        slot,
        run_only,
        frozen_while_held,
    };
    ctx.printer.show(&answer, || {
        let mut text = String::from("Simple commands:");
        for command in &answer.commands {
            let _ = write!(text, "\n  {command}");
        }
        match &answer.slot {
            Some(slot) if answer.run_only => {
                let _ = write!(
                    text,
                    "\nNeeds the slot {slot}, and runs only inside dibs slot run {slot} -- <command>."
                );
            }
            Some(slot) => {
                let _ = write!(text, "\nNeeds the slot {slot}.");
            }
            None => text.push_str("\nNeeds no slot."),
        }
        for slot in &answer.frozen_while_held {
            let _ = write!(
                text,
                "\nIts push is refused while another member holds {slot}."
            );
        }
        text
    });
    Ok(())
}

/// A time as the command line prints it.
pub fn clock(at: OffsetDateTime) -> String {
    at.format(&Rfc3339).unwrap_or_default()
}

/// What a hold's line says: `alice: ship 2.4, until …`.
pub fn hold_text(hold: &SlotHold) -> String {
    let mut text = format!("{} ({})", hold.holder, hold.purpose);
    if hold.stalled {
        let _ = write!(
            text,
            ", stalled: no heartbeat since {}",
            clock(hold.last_seen)
        );
    } else {
        let _ = write!(text, ", until {}", clock(hold.expires_at));
    }
    text
}

/// Where a slot stands, for `me`.
pub fn standing(slot: &Slot, me: &Handle) -> String {
    let mut text = match &slot.hold {
        Some(hold) if hold.holder == *me => format!("held by you, {}", hold_text(hold)),
        Some(hold) if hold.slot != slot.name => {
            format!("held through {} by {}", hold.slot, hold_text(hold))
        }
        Some(hold) => format!("held by {}", hold_text(hold)),
        None => match &slot.offer {
            Some(offer) if offer.member == *me => {
                format!("offered to you until {}", clock(offer.until))
            }
            Some(offer) => format!("offered to {} until {}", offer.member, clock(offer.until)),
            None => "free".to_owned(),
        },
    };
    if !slot.queue.is_empty() {
        let names: Vec<String> = slot.queue.iter().map(|w| w.member.to_string()).collect();
        let _ = write!(text, "; waiting: {}", names.join(", "));
    }
    text
}

/// What a take came to, for `me`.
pub fn took_text(took: &Took, me: &Handle) -> String {
    let name = &took.slot.name;
    match (took.outcome, &took.slot.hold) {
        (TakeOutcome::Taken, Some(hold)) => {
            format!("You hold {name} until {}.", clock(hold.expires_at))
        }
        (TakeOutcome::Renewed, Some(hold)) => {
            format!(
                "Your hold on {name} lasts until {} now.",
                clock(hold.expires_at)
            )
        }
        (TakeOutcome::Taken | TakeOutcome::Renewed, None) => format!("You hold {name}."),
        (TakeOutcome::Queued, _) => {
            let position = took.position.unwrap_or(1);
            format!(
                "{name} is {}; you are number {position} in its queue. Wait for it with dibs slot take {name} --purpose … --wait 9m, or do other work and take it again; never run its command meanwhile.",
                standing(&took.slot, me)
            )
        }
    }
}
