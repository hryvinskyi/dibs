//! The protected branches and the pull-request status of one repository of a settings document,
//! validated and written back.

use std::collections::BTreeSet;

use super::SettingsError;
use super::document::{PrStatusDocument, ProtectedRefDocument};
use crate::handle::Handle;
use crate::pr_status::{PrStatusSettings, StatusContext};
use crate::protection::ProtectedRef;
use crate::repo::RepoId;
use crate::slot::RepoRef;

/// Validates the protected branches of `repo`: each `refs/heads/<branch>`, each once, every pusher
/// a handle.
pub(super) fn parse_protected_refs(
    repo: &RepoId,
    documents: &[ProtectedRefDocument],
) -> Result<Vec<ProtectedRef>, SettingsError> {
    let mut protected: Vec<ProtectedRef> = Vec::with_capacity(documents.len());
    for document in documents {
        let target = RepoRef::of(repo.clone(), &document.target).ok_or_else(|| {
            SettingsError::ProtectedRef {
                repo: repo.clone(),
                text: document.target.clone(),
            }
        })?;
        if protected.iter().any(|known| known.target == target) {
            return Err(SettingsError::DuplicateProtectedRef {
                repo: repo.clone(),
                text: document.target.clone(),
            });
        }
        let pushers = document
            .pushers
            .iter()
            .map(|text| {
                Handle::parse(text).map_err(|_| SettingsError::Pusher {
                    target: target.clone(),
                    text: text.clone(),
                })
            })
            .collect::<Result<BTreeSet<Handle>, _>>()?;
        protected.push(ProtectedRef { target, pushers });
    }
    Ok(protected)
}

/// Validates the pull-request status of `repo`, taking the defaults for what is not written.
pub(super) fn parse_pr_status(
    repo: &RepoId,
    document: Option<&PrStatusDocument>,
) -> Result<PrStatusSettings, SettingsError> {
    let Some(document) = document else {
        return Ok(PrStatusSettings::default());
    };
    let context = document
        .context
        .as_deref()
        .map(|text| {
            StatusContext::parse(text).map_err(|error| SettingsError::StatusContext {
                repo: repo.clone(),
                error,
            })
        })
        .transpose()?
        .unwrap_or_default();
    Ok(PrStatusSettings {
        enabled: document.enabled.unwrap_or(false),
        context,
        describe_holder: document.describe_holder.unwrap_or(false),
    })
}

/// The protected branches written back.
pub(super) fn protected_documents(protected: &[ProtectedRef]) -> Vec<ProtectedRefDocument> {
    protected
        .iter()
        .map(|rule| ProtectedRefDocument {
            target: rule.target.name.clone(),
            pushers: rule.pushers.iter().map(ToString::to_string).collect(),
        })
        .collect()
}

/// The pull-request status written back; nothing when it is the default.
pub(super) fn pr_status_document(settings: &PrStatusSettings) -> Option<PrStatusDocument> {
    (*settings != PrStatusSettings::default()).then(|| PrStatusDocument {
        enabled: Some(settings.enabled),
        context: Some(settings.context.to_string()),
        describe_holder: Some(settings.describe_holder),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hot_rule::HotRuleRegistry;
    use crate::settings::ProjectSettings;
    use crate::testing::document;

    fn parse(json: &str) -> Result<ProjectSettings, SettingsError> {
        ProjectSettings::parse(&document(json), &HotRuleRegistry::standard())
    }

    #[test]
    fn a_repository_protects_branches_for_its_pushers_and_round_trips() {
        let settings = parse(
            r#"{"repos":[
                {"id":"api","remote":"example.com/acme/api",
                 "protected_refs":[{"ref":"refs/heads/main","pushers":["alice"]},
                                   {"ref":"refs/heads/release","pushers":[]}],
                 "pr_status":{"enabled":true,"describe_holder":true}},
                {"id":"web","remote":"example.com/acme/web"}]}"#,
        )
        .unwrap();
        let api = &settings.repos()[0];
        assert_eq!(api.protected_refs.len(), 2);
        assert_eq!(
            api.protected_refs[0].target.to_string(),
            "api:refs/heads/main"
        );
        assert!(api.protected_refs[0].allows(&Handle::parse("alice").unwrap()));
        assert!(api.protected_refs[1].pushers.is_empty());
        assert!(api.pr_status.enabled && api.pr_status.describe_holder);
        assert_eq!(api.pr_status.context.as_str(), "dibs");
        let web = &settings.repos()[1];
        assert!(web.protected_refs.is_empty());
        assert_eq!(web.pr_status, PrStatusSettings::default());
        let again =
            ProjectSettings::parse(&settings.to_document(), &HotRuleRegistry::standard()).unwrap();
        assert_eq!(again, settings);
    }

    #[test]
    fn a_project_with_one_repository_writes_them_at_the_top_level() {
        let settings = parse(
            r#"{"protected_refs":[{"ref":"refs/heads/main","pushers":["alice"]}],
                "pr_status":{"enabled":true,"context":"coordination"}}"#,
        )
        .unwrap();
        let only = &settings.repos()[0];
        assert_eq!(
            only.protected_refs[0].target.to_string(),
            "default:refs/heads/main"
        );
        assert_eq!(only.pr_status.context.as_str(), "coordination");
        let again =
            ProjectSettings::parse(&settings.to_document(), &HotRuleRegistry::standard()).unwrap();
        assert_eq!(again, settings);
        assert_eq!(
            parse(
                r#"{"protected_refs":[{"ref":"refs/heads/main"}],
                    "repos":[{"id":"api","remote":"example.com/acme/api"}]}"#
            ),
            Err(SettingsError::MixedLayout)
        );
    }

    #[test]
    fn protected_refs_are_branches_each_once_with_handles_for_pushers() {
        let repo = RepoId::implicit();
        let written = |target: &str, pushers: &[&str]| ProtectedRefDocument {
            target: target.to_owned(),
            pushers: pushers.iter().map(|text| (*text).to_owned()).collect(),
        };
        assert!(matches!(
            parse_protected_refs(&repo, &[written("main", &[])]),
            Err(SettingsError::ProtectedRef { .. })
        ));
        assert!(matches!(
            parse_protected_refs(&repo, &[written("refs/tags/v1", &[])]),
            Err(SettingsError::ProtectedRef { .. })
        ));
        assert!(matches!(
            parse_protected_refs(
                &repo,
                &[
                    written("refs/heads/main", &[]),
                    written("refs/heads/main", &["alice"])
                ]
            ),
            Err(SettingsError::DuplicateProtectedRef { .. })
        ));
        let error = parse_protected_refs(&repo, &[written("refs/heads/main", &["Alice"])]);
        assert_eq!(
            error.map_err(|error| error.to_string()),
            Err(
                "protected ref 'default:refs/heads/main': pusher 'Alice' is not a member's handle"
                    .to_owned()
            )
        );
        assert!(matches!(
            parse_pr_status(
                &repo,
                Some(&PrStatusDocument {
                    context: Some(" ".to_owned()),
                    ..PrStatusDocument::default()
                })
            ),
            Err(SettingsError::StatusContext { .. })
        ));
    }

    #[test]
    fn a_pull_request_is_found_in_the_repository_whose_remote_it_names() {
        use crate::review::PrRef;

        let listed = parse(
            r#"{"repos":[{"id":"api","remote":"git@example.com:Acme/API.git"},
                         {"id":"web","remote":"example.com/acme/web"}]}"#,
        )
        .unwrap();
        let pr = |text: &str| PrRef::parse(text).unwrap();
        assert_eq!(
            listed
                .repo_of_pr(&pr("acme/api#12"))
                .map(|repo| repo.id.as_str()),
            Some("api")
        );
        assert!(listed.repo_of_pr(&pr("acme/other#1")).is_none());
        assert_eq!(
            listed.repo_named("api").map(|repo| repo.id.as_str()),
            Some("api")
        );
        assert_eq!(
            listed.repo_named("Acme/Web").map(|repo| repo.id.as_str()),
            Some("web")
        );
        assert!(listed.repo_named("acme").is_none());
        let single = parse("{}").unwrap();
        assert_eq!(
            single
                .repo_of_pr(&pr("anyone/anything#3"))
                .map(|repo| repo.id.as_str()),
            Some("default")
        );
        let api = &listed.repos()[0];
        assert_eq!(
            api.branch("main").map(|branch| branch.to_string()),
            Some("api:refs/heads/main".to_owned())
        );
        assert_eq!(
            api.branch("refs/heads/main")
                .map(|branch| branch.to_string()),
            Some("api:refs/heads/main".to_owned())
        );
        assert!(api.branch("refs/tags/v1").is_none());
    }
}
