//! `dibs admin`: members, tokens, settings and forced releases.

use dibs_client::api::{Enrolled, NewMember};
use dibs_domain::member::Role;
use dibs_domain::settings::SettingsDocument;

use std::fmt::Write as _;

use crate::cli::{AdminCommand, AdminThreadCommand, MemberCommand, SettingsCommand, TokenCommand};
use crate::commands::work::show_ended;
use crate::context::Context;
use crate::failure::{Failure, sentence};
use crate::output::item_label;

/// Runs an admin command.
///
/// # Errors
/// The [`Failure`] of the command.
pub async fn run(command: AdminCommand, ctx: &Context) -> Result<(), Failure> {
    let connection = ctx.connect().await?;
    let client = &connection.client;
    match command {
        AdminCommand::Member(MemberCommand::Add(args)) => {
            let member = NewMember {
                handle: args.handle,
                display_name: args.name,
                external_login: args.login,
                role: Role::from(args.role),
            };
            let enrolled = client
                .add_member(&member)
                .await
                .map_err(|error| connection.failed(error))?;
            show_token(ctx, &enrolled, "Added");
        }
        AdminCommand::Member(MemberCommand::Remove(args)) => {
            let removed = client
                .remove_member(&args.handle)
                .await
                .map_err(|error| connection.failed(error))?;
            ctx.printer.show(&removed, || {
                let mut text = format!(
                    "Removed {} from the project: {} token(s) revoked, {} browser session(s) ended. Their messages stay.",
                    removed.member.handle, removed.tokens_revoked, removed.ui_sessions_ended
                );
                if !removed.released.is_empty() {
                    let items: Vec<String> = removed
                        .released
                        .iter()
                        .map(|item| item_label(Some(item), item.id))
                        .collect();
                    let _ = write!(text, "\nReleased: {}.", items.join(", "));
                }
                let slots = &removed.slots;
                for (what, list) in [
                    ("Slots they held", &slots.held),
                    ("Queues they left", &slots.waited),
                    ("Offers passed on", &slots.offered),
                ] {
                    if !list.is_empty() {
                        let names: Vec<&str> = list.iter().map(|slot| slot.as_str()).collect();
                        let _ = write!(text, "\n{what}: {}.", names.join(", "));
                    }
                }
                text
            });
        }
        AdminCommand::Thread(AdminThreadCommand::Delete(args)) => {
            let deleted = client
                .delete_thread(args.id)
                .await
                .map_err(|error| connection.failed(error))?;
            ctx.printer.show(&deleted, || {
                format!(
                    "Deleted conversation #{} with its {} message(s).",
                    deleted.thread_id, deleted.messages
                )
            });
        }
        AdminCommand::Token(TokenCommand::Rotate(args)) => {
            let enrolled = client
                .rotate_token(&args.handle)
                .await
                .map_err(|error| connection.failed(error))?;
            show_token(ctx, &enrolled, "Revoked every token of");
        }
        AdminCommand::Settings(SettingsCommand::Apply(args)) => {
            let text = std::fs::read_to_string(&args.file).map_err(|_| {
                Failure::config(
                    "unreadable_settings",
                    format!("{} cannot be read.", args.file.display()),
                )
            })?;
            let document: SettingsDocument = toml::from_str(&text).map_err(|error| {
                Failure::config(
                    "invalid_settings",
                    sentence(&format!(
                        "{} is not a settings file: {}",
                        args.file.display(),
                        error.message()
                    )),
                )
            })?;
            let applied = client
                .apply_settings(&document)
                .await
                .map_err(|error| connection.failed(error))?;
            ctx.printer.show(&applied, || {
                if applied.changed {
                    "The settings are applied.".to_owned()
                } else {
                    "The settings were already in force; nothing changed.".to_owned()
                }
            });
        }
        AdminCommand::Slot(command) => {
            crate::commands::slot::admin(command, &connection, ctx).await?;
        }
        AdminCommand::ForceRelease(args) => {
            let item = connection.resolve(&args.key).await?;
            let ended = client
                .force_release(item)
                .await
                .map_err(|error| connection.failed(error))?;
            show_ended(ctx, &ended, "Force-released");
        }
    }
    Ok(())
}

fn show_token(ctx: &Context, enrolled: &Enrolled, verb: &str) {
    ctx.printer.show(enrolled, || {
        format!(
            "{verb} {} ({}). Their new token is shown only this once; hand it over through a password manager:\n{}",
            enrolled.member.handle,
            enrolled.member.display_name,
            enrolled.token.expose()
        )
    });
}
