//! Edit verdicts that ask the server only about what it governs: a path no area or hot-file rule
//! of the project's settings matches is allowed without a call, as the server would answer it.

use dibs_domain::paths::{RelPath, RepoPath};
use dibs_domain::repo::RepoId;
use dibs_domain::settings::ProjectSettings;

use crate::api::{EditCheckRequest, PathVerdict, Verdict};
use crate::client::Client;
use crate::error::ClientError;

/// The verdicts on `request.paths`, in the order asked: the ungoverned ones decided from
/// `settings`, the rest by the server in one call. No call is made when nothing is governed.
///
/// # Errors
/// [`ClientError`] of the server's call.
pub async fn edit_verdicts(
    client: &Client,
    settings: &ProjectSettings,
    request: &EditCheckRequest,
) -> Result<Vec<PathVerdict>, ClientError> {
    let governed: Vec<RelPath> = request
        .paths
        .iter()
        .filter(|path| is_governed(settings, &request.repo, path))
        .cloned()
        .collect();
    let mut answered = if governed.is_empty() {
        Vec::new()
    } else {
        let asked = EditCheckRequest {
            paths: governed,
            ..request.clone()
        };
        client.edit_check(&asked).await?.verdicts
    }
    .into_iter();
    Ok(request
        .paths
        .iter()
        .map(|path| {
            if is_governed(settings, &request.repo, path) {
                answered.next()
            } else {
                None
            }
            .unwrap_or_else(|| PathVerdict {
                repo: request.repo.clone(),
                path: path.clone(),
                verdict: Verdict::Allowed,
            })
        })
        .collect())
}

/// Whether an area or a hot-file rule of the repository matches the path.
pub fn is_governed(settings: &ProjectSettings, repo: &RepoId, path: &RelPath) -> bool {
    settings.is_governed(&RepoPath {
        repo: repo.clone(),
        path: path.clone(),
    })
}
