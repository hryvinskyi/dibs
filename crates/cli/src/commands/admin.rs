//! `dibs admin`: members, tokens, settings and forced releases.

use dibs_client::api::{Enrolled, NewMember};
use dibs_domain::member::Role;
use dibs_domain::settings::SettingsDocument;

use crate::cli::{AdminCommand, MemberCommand, SettingsCommand, TokenCommand};
use crate::commands::work::show_ended;
use crate::context::Context;
use crate::failure::{Failure, sentence};

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
