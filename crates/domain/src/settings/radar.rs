//! The published contracts and the area dependencies of one repository of a settings document,
//! validated and written back.

use std::collections::{BTreeMap, BTreeSet};

use super::document::ContractDocument;
use super::{RepoSpec, SettingsError};
use crate::area::{AreaName, QualifiedArea};
use crate::paths::PathGlob;
use crate::radar::{Consumer, Contract};
use crate::repo::RepoId;

/// Validates the contracts of `repo` as written: each with at least one glob, every glob valid,
/// every consumer in one of its forms. Whether a consumer names a declared repository or area is
/// checked once every repository is known, by [`check_references`].
pub(super) fn parse_contracts(
    repo: &RepoId,
    documents: &[ContractDocument],
) -> Result<Vec<Contract>, SettingsError> {
    documents
        .iter()
        .enumerate()
        .map(|(index, document)| {
            let number = index + 1;
            if document.paths.is_empty() {
                return Err(SettingsError::ContractWithoutPaths {
                    repo: repo.clone(),
                    number,
                });
            }
            let paths = document
                .paths
                .iter()
                .map(|glob| {
                    PathGlob::parse(glob).map_err(|error| SettingsError::ContractGlob {
                        repo: repo.clone(),
                        number,
                        glob: glob.clone(),
                        error,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut consumers: Vec<Consumer> = Vec::with_capacity(document.consumers.len());
            for text in &document.consumers {
                let consumer =
                    Consumer::parse(text, repo).map_err(|_| SettingsError::ContractConsumer {
                        repo: repo.clone(),
                        number,
                        text: text.clone(),
                    })?;
                if !consumers.contains(&consumer) {
                    consumers.push(consumer);
                }
            }
            Ok(Contract { paths, consumers })
        })
        .collect()
}

/// Validates the area dependencies of `repo` as written: each key an area `repo` declares, each
/// dependency an area by its name in `repo` or as `repo:area`, never the area itself. Whether a
/// dependency of another repository is declared is checked by [`check_references`].
pub(super) fn parse_area_deps(
    repo: &RepoSpec,
    written: &BTreeMap<String, Vec<String>>,
) -> Result<BTreeMap<AreaName, BTreeSet<QualifiedArea>>, SettingsError> {
    let mut deps = BTreeMap::new();
    for (key, targets) in written {
        let name = AreaName::parse(key)
            .ok()
            .filter(|name| repo.area(name).is_some())
            .ok_or_else(|| SettingsError::AreaDepArea {
                repo: repo.id.clone(),
                area: key.clone(),
            })?;
        let own = repo.qualify(&name);
        let mut set = BTreeSet::new();
        for text in targets {
            let refused = || SettingsError::AreaDep {
                repo: repo.id.clone(),
                area: key.clone(),
                text: text.clone(),
            };
            let target = if text.contains(':') {
                QualifiedArea::parse(text).map_err(|_| refused())?
            } else {
                repo.qualify(&AreaName::parse(text).map_err(|_| refused())?)
            };
            if target == own {
                return Err(refused());
            }
            set.insert(target);
        }
        if !set.is_empty() {
            deps.insert(name, set);
        }
    }
    Ok(deps)
}

/// Checks, once every repository is parsed, that each consumer of a contract names a listed
/// repository (and, for an area, an area it declares) and that each area dependency is a declared
/// area.
pub(super) fn check_references(repos: &[RepoSpec]) -> Result<(), SettingsError> {
    let declared = |area: &QualifiedArea| {
        repos
            .iter()
            .any(|repo| repo.id == area.repo && repo.area(&area.name).is_some())
    };
    for repo in repos {
        for (index, contract) in repo.contracts.iter().enumerate() {
            for consumer in &contract.consumers {
                let known = match consumer {
                    Consumer::Repo(id) => repos.iter().any(|listed| listed.id == *id),
                    Consumer::Area(area) => declared(area),
                };
                if !known {
                    return Err(SettingsError::ContractConsumer {
                        repo: repo.id.clone(),
                        number: index + 1,
                        text: consumer.to_string(),
                    });
                }
            }
        }
        for (name, targets) in &repo.area_deps {
            if let Some(target) = targets.iter().find(|target| !declared(target)) {
                return Err(SettingsError::AreaDep {
                    repo: repo.id.clone(),
                    area: name.to_string(),
                    text: target.to_string(),
                });
            }
        }
    }
    Ok(())
}

/// The contracts written back, each consumer of the contract's own repository by its bare name.
pub(super) fn contract_documents(repo: &RepoSpec) -> Vec<ContractDocument> {
    repo.contracts
        .iter()
        .map(|contract| ContractDocument {
            paths: contract.paths.iter().map(ToString::to_string).collect(),
            consumers: contract
                .consumers
                .iter()
                .map(|consumer| match consumer {
                    Consumer::Area(area) if area.repo == repo.id => area.name.to_string(),
                    other => other.to_string(),
                })
                .collect(),
        })
        .collect()
}

/// The area dependencies written back, each area of the same repository by its bare name.
pub(super) fn area_deps_document(repo: &RepoSpec) -> BTreeMap<String, Vec<String>> {
    repo.area_deps
        .iter()
        .map(|(name, targets)| {
            let written = targets
                .iter()
                .map(|target| {
                    if target.repo == repo.id {
                        target.name.to_string()
                    } else {
                        target.to_string()
                    }
                })
                .collect();
            (name.to_string(), written)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hot_rule::HotRuleRegistry;
    use crate::paths::RelPath;
    use crate::settings::ProjectSettings;
    use crate::testing::{area, document};

    fn parse(json: &str) -> Result<ProjectSettings, SettingsError> {
        ProjectSettings::parse(&document(json), &HotRuleRegistry::standard())
    }

    const TWO: &str = r#"{"repos":[
        {"id":"api","remote":"example.com/acme/api",
         "areas":[{"name":"billing","paths":["packages/billing/**"]},
                  {"name":"search","paths":["packages/search/**"]}],
         "contracts":[{"paths":["packages/*/src/Api/**"]},
                      {"paths":["schema/**"],"consumers":["web:*","search"]}],
         "area_deps":{"search":["billing"]}},
        {"id":"web","remote":"example.com/acme/web",
         "areas":[{"name":"checkout","paths":["src/checkout/**"]}],
         "area_deps":{"checkout":["api:billing"]}}]}"#;

    #[test]
    fn contracts_and_area_dependencies_are_read_per_repository_and_round_trip() {
        let settings = parse(TWO).unwrap();
        let api = &settings.repos()[0];
        assert_eq!(api.contracts.len(), 2);
        assert!(
            api.contracts[0].covers(&RelPath::parse("packages/billing/src/Api/Money.php").unwrap())
        );
        assert_eq!(
            api.contracts[1].consumers,
            [
                Consumer::Repo(RepoId::parse("web").unwrap()),
                Consumer::Area(area("api:search"))
            ]
        );
        assert!(settings.depends(&area("api:search"), &area("api:billing")));
        assert!(settings.depends(&area("web:checkout"), &area("api:billing")));
        assert!(!settings.depends(&area("api:billing"), &area("api:search")));
        let written = settings.to_document();
        assert_eq!(
            written.repos[0].contracts[1].consumers,
            ["web:*", "search"],
            "an area of the contract's own repository is written by its name"
        );
        assert_eq!(written.repos[1].area_deps["checkout"], ["api:billing"]);
        let again = ProjectSettings::parse(&written, &HotRuleRegistry::standard()).unwrap();
        assert_eq!(again, settings);
    }

    #[test]
    fn a_project_with_one_repository_declares_them_at_the_top_level_and_never_beside_repos() {
        let settings = parse(
            r#"{"areas":[{"name":"billing","paths":["billing/**"]},{"name":"search","paths":["search/**"]}],
                "contracts":[{"paths":["billing/api/**"]}],
                "area_deps":{"search":["billing"]}}"#,
        )
        .unwrap();
        assert!(settings.depends(&area("default:search"), &area("default:billing")));
        assert_eq!(settings.repos()[0].contracts.len(), 1);
        let again =
            ProjectSettings::parse(&settings.to_document(), &HotRuleRegistry::standard()).unwrap();
        assert_eq!(again, settings);
        for mixed in [
            r#"{"contracts":[{"paths":["x/**"]}],"repos":[{"id":"api","remote":"example.com/acme/api"}]}"#,
            r#"{"area_deps":{"a":["b"]},"repos":[{"id":"api","remote":"example.com/acme/api"}]}"#,
        ] {
            assert_eq!(parse(mixed), Err(SettingsError::MixedLayout), "{mixed}");
        }
    }

    #[test]
    fn a_contract_or_dependency_naming_what_is_not_declared_is_refused() {
        let areas = r#""areas":[{"name":"billing","paths":["billing/**"]},{"name":"search","paths":["search/**"]}]"#;
        let with = |extra: &str| format!("{{{areas},{extra}}}");
        assert_eq!(
            parse(&with(r#""contracts":[{"paths":[]}]"#)),
            Err(SettingsError::ContractWithoutPaths {
                repo: RepoId::implicit(),
                number: 1
            })
        );
        assert!(matches!(
            parse(&with(r#""contracts":[{"paths":["/abs/**"]}]"#)),
            Err(SettingsError::ContractGlob { number: 1, .. })
        ));
        for consumer in ["Bad", "web:*", "default:checkout"] {
            let json = with(&format!(
                r#""contracts":[{{"paths":["x/**"]}},{{"paths":["y/**"],"consumers":["{consumer}"]}}]"#
            ));
            assert!(
                matches!(
                    parse(&json),
                    Err(SettingsError::ContractConsumer { number: 2, .. })
                ),
                "{consumer}"
            );
        }
        assert_eq!(
            parse(&with(r#""area_deps":{"checkout":["billing"]}"#)),
            Err(SettingsError::AreaDepArea {
                repo: RepoId::implicit(),
                area: "checkout".to_owned()
            })
        );
        for target in [
            "billing:x:y",
            "Bad",
            "search",
            "default:nothing",
            "api:billing",
        ] {
            let json = with(&format!(r#""area_deps":{{"search":["{target}"]}}"#));
            assert!(
                matches!(parse(&json), Err(SettingsError::AreaDep { .. })),
                "{target}: an area never depends on itself or on what is not declared"
            );
        }
        let error = parse(&with(r#""area_deps":{"search":["nothing"]}"#)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "area_deps of repository 'default': area 'search' depends on 'default:nothing', which is not another area the project declares"
        );
    }
}
