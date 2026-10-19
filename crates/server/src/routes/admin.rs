//! `POST /v1/admin/members`, `POST /v1/admin/members/{handle}/tokens`,
//! `DELETE /v1/admin/tokens/{prefix}`, `PUT /v1/admin/settings` and
//! `POST /v1/admin/items/{id}/force-release`: managing a project. Every route refuses a caller who
//! is no admin before it reads the request, so the refusal is the same whatever was sent.

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::{Deserialize, Serialize};

use dibs_app::token::{SecretToken, TokenPrefix};
use dibs_app::use_cases::{CreateMemberInput, NewMember};
use dibs_domain::handle::Handle;
use dibs_domain::member::{DisplayName, ExternalLogin, Role};
use dibs_domain::settings::SettingsDocument;

use crate::auth::Authed;
use crate::error::ApiError;
use crate::fields::{item_in_path, parse, parse_required};
use crate::http::AppState;
use crate::views::{ClaimView, ItemNameView, MemberView, TokenView, reply};

/// The body of `POST /v1/admin/members`.
#[derive(Deserialize)]
pub struct MemberBody {
    handle: Option<String>,
    display_name: Option<String>,
    external_login: Option<String>,
    role: Option<String>,
}

/// A member with the token just issued to them. The token is shown in this answer only; the
/// service keeps its hash.
#[derive(Serialize)]
struct EnrolledAnswer {
    member: MemberView,
    token: String,
}

impl EnrolledAnswer {
    fn of(member: MemberView, token: &SecretToken) -> Self {
        Self {
            member,
            token: token.expose().to_owned(),
        }
    }
}

/// Adds a member to the caller's project with `role` (`member` when absent, `admin`, or
/// `importer` for an integration) and answers `201` with the member and their first token.
///
/// # Errors
/// `forbidden` unless the caller is an admin; `handle_taken` (`409`); `invalid_request` for a
/// missing or invalid field.
pub async fn add_member(
    State(state): State<AppState>,
    Authed(actor): Authed,
    body: Result<Json<MemberBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_admin()?;
    let Json(body) = body?;
    let input = CreateMemberInput {
        member: NewMember {
            handle: parse_required("handle", body.handle.as_deref(), Handle::parse)?,
            display_name: parse_required(
                "display_name",
                body.display_name.as_deref(),
                DisplayName::parse,
            )?,
            external_login: body
                .external_login
                .as_deref()
                .map(|text| parse("external_login", text, ExternalLogin::parse))
                .transpose()?,
        },
        role: body
            .role
            .as_deref()
            .map(|text| parse("role", text, parse_role))
            .transpose()?
            .unwrap_or(Role::Member),
    };
    let created = state.app.create_member.execute(&actor, input).await?;
    Ok(reply(
        StatusCode::CREATED,
        &EnrolledAnswer::of(MemberView::of(&created.member), &created.token),
    ))
}

/// A role by the name it has in every answer.
fn parse_role(text: &str) -> Result<Role, &'static str> {
    match text {
        "admin" => Ok(Role::Admin),
        "member" => Ok(Role::Member),
        "importer" => Ok(Role::Importer),
        _ => Err("a role is admin, member or importer"),
    }
}

/// Revokes every token of the member with this handle and issues a new one, answered `201` with
/// the member. Browser sessions the old tokens opened end with them.
///
/// # Errors
/// `forbidden` unless the caller is an admin; `not_found` for a handle the project has no member
/// of.
pub async fn rotate_token(
    State(state): State<AppState>,
    Authed(actor): Authed,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, ApiError> {
    actor.require_admin()?;
    let Path(text) = path?;
    let handle = Handle::parse(&text).map_err(|_| no_member(&text))?;
    let rotated = state.app.rotate_token.execute(&actor, &handle).await?;
    Ok(reply(
        StatusCode::CREATED,
        &EnrolledAnswer::of(MemberView::of(&rotated.member), &rotated.token),
    ))
}

fn no_member(text: &str) -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "not_found",
        &format!("there is no member '{text}'"),
    )
}

#[derive(Serialize)]
struct TokenAnswer {
    token: TokenView,
}

/// Revokes the project's token whose prefix is `prefix` (the 8 characters after `dibs_`, with or
/// without the marker). Revoking a revoked token changes nothing.
///
/// # Errors
/// `forbidden` unless the caller is an admin; `not_found` for a prefix no token of the project has.
pub async fn revoke_token(
    State(state): State<AppState>,
    Authed(actor): Authed,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, ApiError> {
    actor.require_admin()?;
    let Path(text) = path?;
    let prefix = TokenPrefix::parse(&text).map_err(|_| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "there is no token with that prefix",
        )
    })?;
    let revoked = state.app.revoke_token.execute(&actor, prefix).await?;
    Ok(reply(
        StatusCode::OK,
        &TokenAnswer {
            token: TokenView::of(revoked),
        },
    ))
}

#[derive(Serialize)]
struct SettingsAnswer {
    /// The settings in force, in their normal form.
    settings: SettingsDocument,
    /// Whether they differ from the ones before.
    changed: bool,
}

/// Makes the body, a settings document, the project's settings, and answers them in their normal
/// form. Applying the settings in force again changes nothing.
///
/// # Errors
/// `forbidden` unless the caller is an admin; `invalid_settings` naming the first setting at fault;
/// `invalid_json` for a body that is no settings document.
pub async fn apply_settings(
    State(state): State<AppState>,
    Authed(actor): Authed,
    body: Result<Json<SettingsDocument>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_admin()?;
    let Json(document) = body?;
    let applied = state.app.apply_settings.execute(&actor, document).await?;
    Ok(reply(
        StatusCode::OK,
        &SettingsAnswer {
            settings: applied.settings.to_document(),
            changed: applied.changed,
        },
    ))
}

#[derive(Serialize)]
struct ReleasedAnswer {
    /// The claim that ended.
    claim: ClaimView,
    /// The item it was on.
    item: ItemNameView,
}

/// Ends any member's claim on the item; the item is open again.
///
/// # Errors
/// `forbidden` unless the caller is an admin; `not_found` for an id the project has no item of;
/// `no_claim` (`409`) when nobody holds the item.
pub async fn force_release(
    State(state): State<AppState>,
    Authed(actor): Authed,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, ApiError> {
    actor.require_admin()?;
    let Path(id) = path?;
    let item = item_in_path(&id)?;
    let ended = state.app.force_release.execute(&actor, item).await?;
    Ok(reply(
        StatusCode::OK,
        &ReleasedAnswer {
            claim: ClaimView::of(ended.claim),
            item: ItemNameView::of(ended.item),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_role_has_its_name() {
        for role in [Role::Admin, Role::Member, Role::Importer] {
            let name = serde_json::to_value(role).unwrap();
            assert_eq!(parse_role(name.as_str().unwrap()), Ok(role));
        }
        assert!(parse_role("Admin").is_err());
        assert!(parse_role("").is_err());
    }
}
