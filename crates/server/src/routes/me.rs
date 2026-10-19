//! `GET /v1/me`: the caller, their project, and its settings.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use serde::Serialize;

use dibs_domain::project::ProjectSlug;
use dibs_domain::repo::{RemoteUrl, RepoId};
use dibs_domain::settings::SettingsDocument;

use crate::auth::Authed;
use crate::error::ApiError;
use crate::http::AppState;
use crate::views::{MemberView, reply};

#[derive(Serialize)]
struct MeAnswer {
    member: MemberView,
    project: ProjectView,
    repos: Vec<RepoView>,
    settings: SettingsDocument,
}

#[derive(Serialize)]
struct ProjectView {
    slug: ProjectSlug,
}

/// A repository a checkout's configuration may name; the one repository of a project that lists
/// none has no remote.
#[derive(Serialize)]
struct RepoView {
    id: RepoId,
    remote: Option<RemoteUrl>,
}

/// Answers who the caller is: their member record, their project's slug, the repositories a
/// checkout may name, and the settings in their normal form, which parse back to the same settings.
///
/// # Errors
/// The answer for the use case's failure.
pub async fn me(
    State(state): State<AppState>,
    Authed(actor): Authed,
) -> Result<Response, ApiError> {
    let view = state.app.me.execute(&actor).await?;
    let answer = MeAnswer {
        member: MemberView::of(&view.member),
        project: ProjectView {
            slug: view.project.slug,
        },
        repos: view
            .settings
            .repos()
            .iter()
            .map(|repo| RepoView {
                id: repo.id.clone(),
                remote: repo.remote.clone(),
            })
            .collect(),
        settings: view.settings.to_document(),
    };
    Ok(reply(StatusCode::OK, &answer))
}
