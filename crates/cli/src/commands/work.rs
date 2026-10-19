//! Work items and claims: `status`, `items`, `new`, `claim`, `takeover`, `release`, `done` and
//! `grant`.

use std::fmt::Write as _;

use time::OffsetDateTime;

use dibs_client::api::{
    ClaimOutcome, ClaimRequest, Ended, GrantRequest, HeldItem, ItemFilter, Items, NewItem,
    TakeOverRequest,
};
use dibs_domain::message::Body;

use crate::cli::{ClaimArgs, GrantArgs, ItemsArgs, KeyArgs, NewArgs, TakeoverArgs};
use crate::context::Context;
use crate::failure::{Failure, sentence};
use crate::output::{claim_text, item_line};

/// Shows the live and stale claims and the areas they hold.
///
/// # Errors
/// The [`Failure`] of the call.
pub async fn status(ctx: &Context) -> Result<(), Failure> {
    let connection = ctx.connect().await?;
    let status = connection
        .client
        .status()
        .await
        .map_err(|error| connection.failed(error))?;
    if let Err(error) = connection
        .state
        .remember_status(&status, OffsetDateTime::now_utc())
    {
        tracing::debug!(error = %error, "the status could not be cached");
    }
    ctx.printer.show(&status, || {
        let mut text = String::new();
        let section = |text: &mut String, title: &str, held: &[HeldItem]| {
            if held.is_empty() {
                return;
            }
            let _ = writeln!(text, "{title}:");
            for entry in held {
                let _ = writeln!(
                    text,
                    "  {} — {}",
                    item_line(&entry.item),
                    claim_text(&entry.claim)
                );
            }
        };
        section(&mut text, "Live claims", &status.live);
        section(&mut text, "Stale claims", &status.stale);
        if text.is_empty() {
            text.push_str("Nobody holds anything.");
        }
        text
    });
    Ok(())
}

/// Lists work items: all of them, the caller's, or those in one state.
///
/// # Errors
/// The [`Failure`] of the call.
pub async fn items(args: ItemsArgs, ctx: &Context) -> Result<(), Failure> {
    let connection = ctx.connect().await?;
    let filter = ItemFilter {
        status: args.status.map(Into::into),
        holder: args.mine.then(|| connection.me.member.handle.clone()),
        external_ref: None,
    };
    let items = connection
        .client
        .items(&filter)
        .await
        .map_err(|error| connection.failed(error))?;
    let answer = Items { items };
    ctx.printer.show(&answer, || {
        if answer.items.is_empty() {
            return "No work items match.".to_owned();
        }
        answer
            .items
            .iter()
            .map(item_line)
            .collect::<Vec<_>>()
            .join("\n")
    });
    Ok(())
}

/// Creates the item a reference names, or shows the one that exists.
///
/// # Errors
/// The [`Failure`] of the call.
pub async fn new(args: NewArgs, ctx: &Context) -> Result<(), Failure> {
    let connection = ctx.connect().await?;
    let created = connection
        .client
        .create_item(&NewItem {
            external_ref: Some(args.reference),
            title: args.title,
        })
        .await
        .map_err(|error| connection.failed(error))?;
    ctx.printer.show(&created, || {
        let line = item_line(&created.item);
        if created.created {
            format!("Created {line}.")
        } else {
            format!("Already there: {line}.")
        }
    });
    Ok(())
}

/// Claims an item with the areas the caller will change, or renews the caller's claim.
///
/// # Errors
/// A refusal naming the member in the way; the [`Failure`] of the call.
pub async fn claim(args: ClaimArgs, ctx: &Context) -> Result<(), Failure> {
    let connection = ctx.connect().await?;
    let item = connection.resolve(&args.key).await?;
    let session = connection.session().await?;
    let claimed = connection
        .client
        .claim(
            item,
            &ClaimRequest {
                session_id: session,
                areas: args.areas,
            },
        )
        .await
        .map_err(|error| connection.failed(error))?;
    ctx.printer.show(&claimed, || {
        let verb = match claimed.outcome {
            ClaimOutcome::New => "Claimed",
            ClaimOutcome::Renewed => "Renewed your claim on",
        };
        let mut text = format!("{verb} #{} ({}).", item, claim_text(&claimed.claim));
        if !claimed.blocked_by.is_empty() {
            let blockers: Vec<String> = claimed.blocked_by.iter().map(|b| b.label()).collect();
            let _ = write!(
                text,
                " It is blocked by {}, which is not done yet; check with whoever works on it.",
                blockers.join(", ")
            );
        }
        if !claimed.dropped_areas.is_empty() {
            let dropped: Vec<String> = claimed
                .dropped_areas
                .iter()
                .map(ToString::to_string)
                .collect();
            let _ = write!(text, " No longer held: {}.", dropped.join(", "));
        }
        text
    });
    Ok(())
}

/// Takes over another member's stale claim, telling them why.
///
/// # Errors
/// A refusal when the claim is not stale; the [`Failure`] of the call.
pub async fn take_over(args: TakeoverArgs, ctx: &Context) -> Result<(), Failure> {
    let reason = Body::parse(&args.reason).map_err(|error| {
        Failure::config(
            "invalid_reason",
            sentence(&format!("the reason is not valid: {error}")),
        )
    })?;
    let connection = ctx.connect().await?;
    let item = connection.resolve(&args.key).await?;
    let session = connection.session().await?;
    let taken = connection
        .client
        .take_over(
            item,
            &TakeOverRequest {
                session_id: session,
                reason,
                areas: args.areas,
            },
        )
        .await
        .map_err(|error| connection.failed(error))?;
    ctx.printer.show(&taken, || {
        format!(
            "Took over #{item} from {}; they were told why in conversation #{}.",
            taken.previous_holder, taken.thread_id
        )
    });
    Ok(())
}

/// Releases the caller's claim.
///
/// # Errors
/// The [`Failure`] of the call.
pub async fn release(args: KeyArgs, ctx: &Context) -> Result<(), Failure> {
    let connection = ctx.connect().await?;
    let item = connection.resolve(&args.key).await?;
    let ended = connection
        .client
        .release(item)
        .await
        .map_err(|error| connection.failed(error))?;
    show_ended(ctx, &ended, "Released");
    Ok(())
}

/// Completes the caller's claim.
///
/// # Errors
/// The [`Failure`] of the call.
pub async fn done(args: KeyArgs, ctx: &Context) -> Result<(), Failure> {
    let connection = ctx.connect().await?;
    let item = connection.resolve(&args.key).await?;
    let ended = connection
        .client
        .done(item)
        .await
        .map_err(|error| connection.failed(error))?;
    show_ended(ctx, &ended, "Completed");
    Ok(())
}

/// Prints a claim that ended.
pub fn show_ended(ctx: &Context, ended: &Ended, verb: &str) {
    ctx.printer.show(ended, || {
        format!(
            "{verb} #{} ({}).",
            ended.claim.item,
            claim_text(&ended.claim)
        )
    });
}

/// Lets another member edit inside an area the caller holds, for the grantee's item.
///
/// # Errors
/// The [`Failure`] of the call.
pub async fn grant(args: GrantArgs, ctx: &Context) -> Result<(), Failure> {
    let connection = ctx.connect().await?;
    let item = connection.resolve(&args.key).await?;
    let granted = connection
        .client
        .grant(&GrantRequest {
            item,
            area: args.area,
            to: args.to,
        })
        .await
        .map_err(|error| connection.failed(error))?;
    ctx.printer.show(&granted, || {
        format!(
            "{} may edit in {} for #{}.",
            granted.grant.to, granted.grant.area, granted.grant.item
        )
    });
    Ok(())
}
