//! A branch a claim names, as a person writes it: `[<repo>:]<branch>`. Git refuses `:` in a branch
//! name, so the first `:` always ends the repository. Without a repository, the branch is in the
//! repository of the checkout the command runs in, or the project's only one.

use std::collections::BTreeMap;

use dibs_client::api::Me;
use dibs_domain::radar::BranchName;

use crate::commands::check::{Governed, settings_of};
use crate::context::Connection;
use crate::failure::{Failure, sentence};

/// A branch a claim names, and the repository it is in when the person says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedBranch {
    repo: Option<String>,
    branch: BranchName,
}

impl NamedBranch {
    /// Reads `<branch>` or `<repo>:<branch>`, the repository named by its id (`api`) or its
    /// remote's path (`acme/api`).
    ///
    /// # Errors
    /// A sentence saying what to change when a side is empty or the branch is no branch git
    /// accepts.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let (repo, branch) = match text.split_once(':') {
            Some((repo, branch)) => (Some(repo.trim()), branch.trim()),
            None => (None, text),
        };
        if repo.is_some_and(str::is_empty) {
            return Err(format!(
                "'{text}' names no repository before ':'; write <repo>:<branch>, or the branch alone"
            ));
        }
        if branch.is_empty() {
            return Err(format!(
                "'{text}' names no branch; write <repo>:<branch>, or the branch alone"
            ));
        }
        let branch = BranchName::parse(branch).map_err(|error| error.to_string())?;
        Ok(Self {
            repo: repo.map(str::to_owned),
            branch,
        })
    }
}

/// The branches as a claim sends them, by repository as the person named it; a branch named
/// without one is in the repository of the connection's checkout, or the project's only one.
///
/// # Errors
/// A configuration [`Failure`] when one repository is given two branches, or a branch names no
/// repository where neither a checkout nor a project of one repository says which it is.
pub fn branches(
    named: &[NamedBranch],
    connection: &Connection,
    me: &Me,
) -> Result<BTreeMap<String, BranchName>, Failure> {
    let mut sent = BTreeMap::new();
    let mut here = None;
    for one in named {
        let repo = match &one.repo {
            Some(repo) => repo.clone(),
            None => {
                let repo = match here.take() {
                    Some(repo) => repo,
                    None => default_repo(connection, me)?,
                };
                here = Some(repo.clone());
                repo
            }
        };
        if let Some(earlier) = sent.insert(repo.clone(), one.branch.clone())
            && earlier != one.branch
        {
            return Err(Failure::config(
                "invalid_arguments",
                sentence(&format!(
                    "the repository {repo} is given two branches, {earlier} and {}; name one",
                    one.branch
                )),
            ));
        }
    }
    Ok(sent)
}

/// The repository a branch named without one is in: the one the checkout is, its remote
/// confirmed as an edit check confirms it, or the project's only repository outside a checkout.
fn default_repo(connection: &Connection, me: &Me) -> Result<String, Failure> {
    if connection.checkout.is_some() {
        return Ok(Governed::of(connection, me)?.repo().to_string());
    }
    match settings_of(me)?.repos() {
        [only] => Ok(only.id.to_string()),
        _ => Err(Failure::config(
            "invalid_arguments",
            concat!(
                "This project has several repositories and no checkout is here to say which a ",
                "branch is in: name it as <repo>:<branch>."
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(repo: Option<&str>, branch: &str) -> NamedBranch {
        NamedBranch {
            repo: repo.map(str::to_owned),
            branch: BranchName::parse(branch).unwrap(),
        }
    }

    #[test]
    fn a_branch_is_read_with_its_repository_or_alone() {
        assert_eq!(
            NamedBranch::parse("bob/42-refunds"),
            Ok(named(None, "bob/42-refunds"))
        );
        assert_eq!(
            NamedBranch::parse("api:bob/42-refunds"),
            Ok(named(Some("api"), "bob/42-refunds"))
        );
        assert_eq!(
            NamedBranch::parse(" acme/api : bob/42-refunds "),
            Ok(named(Some("acme/api"), "bob/42-refunds"))
        );
    }

    #[test]
    fn an_empty_side_or_a_branch_git_refuses_is_refused() {
        let no_repo = NamedBranch::parse(":bob/42-refunds").unwrap_err();
        assert!(no_repo.contains("names no repository"), "{no_repo}");
        let no_branch = NamedBranch::parse("api:").unwrap_err();
        assert!(no_branch.contains("names no branch"), "{no_branch}");
        let nothing = NamedBranch::parse("  ").unwrap_err();
        assert!(nothing.contains("names no branch"), "{nothing}");
        let two_colons = NamedBranch::parse("api:bob:42").unwrap_err();
        assert!(two_colons.contains("not a branch name"), "{two_colons}");
        assert!(NamedBranch::parse("api:bob/../42").is_err());
    }
}
