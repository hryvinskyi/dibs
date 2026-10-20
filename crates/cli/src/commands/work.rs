//! Work items and claims: `status`, `items`, `new`, `claim`, `takeover`, `release`, `done` and
//! `grant`.

use std::fmt::Write as _;

use time::OffsetDateTime;

use dibs_client::api::{
    ClaimOutcome, ClaimRequest, DropPrRequest, Ended, GrantRequest, HeldItem, ItemFilter, Items,
    NewItem, ReviewRequest, TakeOverRequest, WideAreaState, WideClaim, WideRequest,
};
use dibs_domain::message::Body;
use dibs_domain::wide::WideReason;

use crate::cli::{
    ClaimAction, ClaimArgs, GrantArgs, ItemsArgs, KeyArgs, NewArgs, ReviewArgs, TakeoverArgs,
};
use crate::context::Context;
use crate::failure::{Failure, sentence};
use crate::output::{claim_text, escalation_line, item_label, item_line};

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
        if !status.wide.is_empty() {
            let _ = writeln!(
                text,
                "Wide claims (they hold none of their areas; a holder's acknowledgement grants the edit):"
            );
            for wide in &status.wide {
                let _ = writeln!(text, "  {}", wide_line(wide));
            }
        }
        if !status.escalations.is_empty() {
            let _ = writeln!(text, "Escalations waiting for an arbiter:");
            for escalation in &status.escalations {
                let _ = writeln!(text, "  {}", escalation_line(escalation));
            }
        }
        if text.is_empty() {
            text.push_str("Nobody holds anything.");
        }
        if let Some(since) = status.integration_silent_since {
            let _ = write!(
                text,
                "\nThe integration has been silent since {}: merges complete no claims and pull request statuses go stale until it calls again.",
                crate::commands::slot::clock(since)
            );
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
    if let Some(ClaimAction::Review(review)) = args.action {
        return start_review(review, ctx).await;
    }
    let key = args.key.ok_or_else(|| {
        Failure::config(
            "invalid_arguments",
            "Name the work item to claim: dibs claim <item>.".to_owned(),
        )
    })?;
    let wide = match args.reason {
        Some(reason) => {
            WideReason::parse(&reason).map_err(|error| {
                Failure::config(
                    "invalid_reason",
                    sentence(&format!("the reason is not valid: {error}")),
                )
            })?;
            Some(WideRequest {
                areas: args.wide,
                reason,
            })
        }
        None => None,
    };
    let connection = ctx.connect().await?;
    let item = connection.resolve(&key).await?;
    let session = connection.session().await?;
    let claimed = connection
        .client
        .claim(
            item,
            &ClaimRequest {
                session_id: session,
                areas: args.areas,
                wide,
            },
        )
        .await
        .map_err(|error| connection.failed(error))?;
    ctx.printer.show(&claimed, || {
        let verb = match claimed.outcome {
            ClaimOutcome::New => "Claimed",
            ClaimOutcome::Renewed => "Renewed your claim on",
        };
        let mut text = format!(
            "{verb} {} ({}).",
            item_label(claimed.item.as_ref(), item),
            claim_text(&claimed.claim)
        );
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
        if let Some(scope) = &claimed.claim.wide {
            let areas: Vec<String> = scope.areas.iter().map(ToString::to_string).collect();
            let _ = write!(
                text,
                " Wide over {}, holding none of them: you may edit where nobody holds the area.",
                areas.join(", ")
            );
        }
        for asked in &claimed.announced {
            let areas: Vec<String> = asked.areas.iter().map(ToString::to_string).collect();
            let _ = write!(
                text,
                " Asked {} to acknowledge (message {}) for {}; your edits there wait for it.",
                asked.to,
                asked.message_id,
                areas.join(", ")
            );
        }
        for inside in &claimed.inside_wide {
            let _ = write!(
                text,
                " {} is in {}'s wide claim on {}",
                inside.area,
                inside.holder,
                inside.item.label()
            );
            match inside.message_id {
                Some(message) => {
                    let _ = write!(
                        text,
                        "; read message {message} and acknowledge it (dibs ack {message}) to let {} edit there.",
                        inside.holder
                    );
                }
                None => text.push('.'),
            }
        }
        text
    });
    Ok(())
}

/// Records a pull request on the caller's claim, which waits in review from then on, or removes
/// one recorded by mistake.
///
/// # Errors
/// A refusal when the caller holds no live claim on the item, another claim recorded the pull
/// request, the project lists no repository of it, or the claim did not record the one to remove;
/// the [`Failure`] of the call.
async fn start_review(args: ReviewArgs, ctx: &Context) -> Result<(), Failure> {
    let connection = ctx.connect().await?;
    let item = connection.resolve(&args.key).await?;
    let reviewed = match (args.pr, args.drop_pr) {
        (Some(pr), _) => {
            connection
                .client
                .review(
                    item,
                    &ReviewRequest {
                        pr,
                        more: args.more,
                    },
                )
                .await
        }
        (None, Some(pr)) => connection.client.drop_pr(item, &DropPrRequest { pr }).await,
        (None, None) => {
            return Err(Failure::config(
                "invalid_arguments",
                "Name the pull request to record (--pr) or to remove (--drop-pr).".to_owned(),
            ));
        }
    }
    .map_err(|error| connection.failed(error))?;
    ctx.printer.show(&reviewed, || {
        if !reviewed.claim.in_review() {
            return format!(
                "Your claim on {} is no longer in review: you work on it again, with its areas.",
                item_label(Some(&reviewed.item), item)
            );
        }
        let prs: Vec<String> = reviewed
            .claim
            .review
            .as_ref()
            .map(|review| review.prs.iter().map(ToString::to_string).collect())
            .unwrap_or_default();
        let ends = if reviewed.claim.review.as_ref().is_some_and(|review| review.more) {
            "its merge returns the claim to you for the next pull request"
        } else {
            "the merge of its last pull request completes it"
        };
        format!(
            "Your claim on {} is in review on {}: it keeps its areas and needs no heartbeat while the pull request is open, and {ends}.",
            item_label(Some(&reviewed.item), item),
            prs.join(", ")
        )
    });
    Ok(())
}

/// One line for a live wide claim: its item, member, reason and where each area stands.
fn wide_line(wide: &WideClaim) -> String {
    let reason = wide
        .claim
        .wide
        .as_ref()
        .map(|scope| scope.reason.as_str())
        .unwrap_or_default();
    let areas: Vec<String> = wide
        .areas
        .iter()
        .map(|area| match (area.state, &area.holder) {
            (WideAreaState::Granted, Some(holder)) => {
                format!("{} (acknowledged by {holder})", area.area)
            }
            (WideAreaState::Waiting, Some(holder)) => {
                format!("{} (waiting for {holder})", area.area)
            }
            _ => format!("{} (free)", area.area),
        })
        .collect();
    format!(
        "{} — {}: {reason}; {}",
        item_line(&wide.item),
        wide.claim.holder,
        areas.join(", ")
    )
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
            "Took over {} from {}; they were told why in conversation #{}.",
            item_label(taken.item.as_ref(), item),
            taken.previous_holder,
            taken.thread_id
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
            "{verb} {} ({}).",
            item_label(ended.item.as_ref(), ended.claim.item),
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
            "{} may edit in {} for {}.",
            granted.grant.to,
            granted.grant.area,
            item_label(granted.item.as_ref(), granted.grant.item)
        )
    });
    Ok(())
}
