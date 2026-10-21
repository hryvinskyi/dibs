//! The protected refs and the pull-request status of one repository of a settings document,
//! validated and written back.

use std::collections::BTreeSet;

use super::SettingsError;
use super::document::{PrStatusDocument, ProtectedRefDocument};
use crate::handle::Handle;
use crate::pr_status::{PrStatusSettings, StatusContext};
use crate::protection::{ProtectedRef, ProtectedTarget};
use crate::repo::RepoId;

/// Validates the protected refs of `repo`: each a branch `refs/heads/<branch>` or tags
/// `refs/tags/<name>` or `refs/tags/<prefix>*`, each once, every pusher a handle.
pub(super) fn parse_protected_refs(
    repo: &RepoId,
    documents: &[ProtectedRefDocument],
) -> Result<Vec<ProtectedRef>, SettingsError> {
    let mut protected: Vec<ProtectedRef> = Vec::with_capacity(documents.len());
    for document in documents {
        let target = ProtectedTarget::parse(repo.clone(), &document.target).map_err(|error| {
            SettingsError::ProtectedRef {
                repo: repo.clone(),
                text: document.target.clone(),
                error,
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

/// The protected refs written back.
pub(super) fn protected_documents(protected: &[ProtectedRef]) -> Vec<ProtectedRefDocument> {
    protected
        .iter()
        .map(|rule| ProtectedRefDocument {
            target: rule.target.name().to_owned(),
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
    use crate::protection::RefKind;
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
    fn tags_are_protected_by_name_or_by_a_trailing_star_and_round_trip() {
        let settings = parse(
            r#"{"repos":[
                {"id":"api","remote":"example.com/acme/api",
                 "protected_refs":[{"ref":"refs/heads/main","pushers":["alice"]},
                                   {"ref":"refs/tags/v*","pushers":["carol"]},
                                   {"ref":"refs/tags/stable","pushers":[]}]}]}"#,
        )
        .unwrap();
        let api = &settings.repos()[0];
        let names: Vec<String> = api
            .protected_refs
            .iter()
            .map(|rule| rule.target.to_string())
            .collect();
        assert_eq!(
            names,
            [
                "api:refs/heads/main",
                "api:refs/tags/v*",
                "api:refs/tags/stable"
            ]
        );
        assert_eq!(api.protected_refs[1].target.kind(), RefKind::Tag);
        assert!(api.protected_refs[1].target.covers("refs/tags/v2.4.0"));
        assert!(api.protects_branch());
        let document = settings.to_document();
        let written = &document.repos[0].protected_refs;
        assert_eq!(written[1].target, "refs/tags/v*");
        assert_eq!(written[1].pushers, ["carol"]);
        let again = ProjectSettings::parse(&document, &HotRuleRegistry::standard()).unwrap();
        assert_eq!(again, settings);

        let tags_only =
            parse(r#"{"protected_refs":[{"ref":"refs/tags/v*","pushers":["carol"]}]}"#).unwrap();
        assert!(
            !tags_only.repos()[0].protects_branch(),
            "protected tags protect no branch"
        );
    }

    #[test]
    fn without_protected_tags_the_branches_and_everything_else_stay() {
        let settings = parse(
            r#"{"stale_after":"2h",
                "protected_refs":[{"ref":"refs/tags/v*","pushers":["carol"]},
                                  {"ref":"refs/heads/main","pushers":["alice"]}]}"#,
        )
        .unwrap();
        let older = settings.without_protected_tags();
        let rules: Vec<String> = older.repos()[0]
            .protected_refs
            .iter()
            .map(|rule| rule.target.to_string())
            .collect();
        assert_eq!(rules, ["default:refs/heads/main"]);
        assert_eq!(older.stale_after(), settings.stale_after());
        let mut expected = settings.to_document();
        expected.protected_refs.remove(0);
        assert_eq!(older.to_document(), expected);
        let none = parse(r#"{"stale_after":"2h"}"#).unwrap();
        assert_eq!(none.without_protected_tags(), none);
    }

    #[test]
    fn a_tag_pattern_with_a_star_inside_two_stars_or_nothing_is_refused_by_name() {
        let refused = |text: &str| {
            parse(&format!(r#"{{"protected_refs":[{{"ref":"{text}"}}]}}"#))
                .map_err(|error| error.to_string())
                .unwrap_err()
        };
        let inside = refused("refs/tags/v*.0");
        assert!(
            inside.starts_with("protected ref 'refs/tags/v*.0' of repository 'default':")
                && inside.contains("'*' at the end only"),
            "{inside}"
        );
        let double = refused("refs/tags/v**");
        assert!(double.contains("'**' is not accepted"), "{double}");
        let empty = refused("refs/tags/");
        assert!(empty.contains("after 'refs/tags/'"), "{empty}");
        let branch_glob = refused("refs/heads/release/*");
        assert!(branch_glob.contains("one branch exactly"), "{branch_glob}");
        assert!(matches!(
            parse_protected_refs(
                &RepoId::implicit(),
                &[
                    ProtectedRefDocument {
                        target: "refs/tags/v*".to_owned(),
                        pushers: Vec::new(),
                    },
                    ProtectedRefDocument {
                        target: "refs/tags/v*".to_owned(),
                        pushers: vec!["alice".to_owned()],
                    },
                ]
            ),
            Err(SettingsError::DuplicateProtectedRef { .. })
        ));
    }

    #[test]
    fn protected_refs_are_branches_or_tags_each_once_with_handles_for_pushers() {
        let repo = RepoId::implicit();
        let written = |target: &str, pushers: &[&str]| ProtectedRefDocument {
            target: target.to_owned(),
            pushers: pushers.iter().map(|text| (*text).to_owned()).collect(),
        };
        assert!(matches!(
            parse_protected_refs(&repo, &[written("main", &[])]),
            Err(SettingsError::ProtectedRef { .. })
        ));
        assert!(parse_protected_refs(&repo, &[written("refs/tags/v1", &[])]).is_ok());
        assert!(matches!(
            parse_protected_refs(&repo, &[written("refs/notes/x", &[])]),
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
