//! Forges: the addresses of the web pages a forge shows for a repository — an issue or pull
//! request, a branch, a file or directory at a revision, a commit — per forge, chosen by the host
//! of the repository's remote.

use std::collections::BTreeMap;
use std::fmt;

use crate::radar::BranchName;
use crate::repo::RemoteUrl;

/// A forge's web pages for a repository: an issue or pull request, a branch, a file or directory at
/// a revision, a commit.
pub trait Forge: Send + Sync + fmt::Debug {
    /// The forge's name as a person reads it, such as "GitHub".
    fn name(&self) -> &'static str;

    /// The page of issue or pull request `number`; a forge that tells them apart redirects to the
    /// right one.
    fn issue(&self, repo: &RemoteUrl, number: u64) -> String;

    /// The page of pull request `number`.
    fn pull_request(&self, repo: &RemoteUrl, number: u64) -> String;

    /// The page of a branch.
    fn branch(&self, repo: &RemoteUrl, branch: &BranchName) -> String;

    /// The page of a file at a revision.
    fn file(&self, repo: &RemoteUrl, at: Revision<'_>, path: &str) -> String;

    /// The page of a directory at a revision; an empty path is the repository's root.
    fn directory(&self, repo: &RemoteUrl, at: Revision<'_>, path: &str) -> String;

    /// The page of a commit.
    fn commit(&self, repo: &RemoteUrl, sha: &str) -> String;
}

/// Where in a repository's history a page shows a file or directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revision<'a> {
    /// The repository's default branch, whatever it is named.
    DefaultBranch,
    /// A branch by its name.
    Branch(&'a BranchName),
}

/// The forges a deployment knows, by the host of a repository's remote. A host it does not know
/// has no pages.
#[derive(Debug, Default)]
pub struct ForgeRegistry {
    forges: BTreeMap<String, Box<dyn Forge>>,
}

impl ForgeRegistry {
    /// A registry that knows no forge.
    pub fn empty() -> Self {
        Self::default()
    }

    /// The forges this crate ships: `github.com`.
    pub fn standard() -> Self {
        Self::empty().with("github.com", Box::new(GitHub))
    }

    /// The registry with `forge` registered for the host `host`, compared without regard to case,
    /// replacing an earlier one of that host.
    #[must_use]
    pub fn with(mut self, host: &str, forge: Box<dyn Forge>) -> Self {
        self.forges.insert(host.to_ascii_lowercase(), forge);
        self
    }

    /// The forge of a remote's host, when the registry knows it.
    pub fn of(&self, remote: &RemoteUrl) -> Option<&dyn Forge> {
        self.forges.get(remote.host()).map(AsRef::as_ref)
    }
}

/// github.com, and any host registered as GitHub.
#[derive(Debug, Clone, Copy, Default)]
pub struct GitHub;

impl GitHub {
    /// The address of the repository's own page, which every other page extends.
    fn home(repo: &RemoteUrl) -> String {
        format!("https://{}/{}", repo.host(), repo.path())
    }

    /// The revision as GitHub's addresses write it: `HEAD` for the default branch, which GitHub
    /// resolves to whatever that branch is named.
    fn revision(at: Revision<'_>) -> String {
        match at {
            Revision::DefaultBranch => "HEAD".to_owned(),
            Revision::Branch(branch) => encode(branch.as_str()),
        }
    }
}

impl Forge for GitHub {
    fn name(&self) -> &'static str {
        "GitHub"
    }

    fn issue(&self, repo: &RemoteUrl, number: u64) -> String {
        format!("{}/issues/{number}", Self::home(repo))
    }

    fn pull_request(&self, repo: &RemoteUrl, number: u64) -> String {
        format!("{}/pull/{number}", Self::home(repo))
    }

    fn branch(&self, repo: &RemoteUrl, branch: &BranchName) -> String {
        format!("{}/tree/{}", Self::home(repo), encode(branch.as_str()))
    }

    fn file(&self, repo: &RemoteUrl, at: Revision<'_>, path: &str) -> String {
        format!(
            "{}/blob/{}/{}",
            Self::home(repo),
            Self::revision(at),
            encode(in_repository(path))
        )
    }

    fn directory(&self, repo: &RemoteUrl, at: Revision<'_>, path: &str) -> String {
        let root = format!("{}/tree/{}", Self::home(repo), Self::revision(at));
        match in_repository(path) {
            "" => root,
            path => format!("{root}/{}", encode(path)),
        }
    }

    fn commit(&self, repo: &RemoteUrl, sha: &str) -> String {
        format!("{}/commit/{}", Self::home(repo), encode(sha))
    }
}

/// A path inside a repository without the leading `/`s it may be written with.
fn in_repository(path: &str) -> &str {
    path.trim_start_matches('/')
}

/// `text` percent-encoded byte by byte as UTF-8, keeping `/` and the unreserved characters
/// `A–Z a–z 0–9 - . _ ~`, so a branch name or a path stays one part of an address whatever it
/// holds.
fn encode(text: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn widgets() -> RemoteUrl {
        RemoteUrl::parse("https://github.com/acme/widgets.git").unwrap()
    }

    fn branch(name: &str) -> BranchName {
        BranchName::parse(name).unwrap()
    }

    #[test]
    fn github_writes_each_page_of_a_repository() {
        let repo = widgets();
        let main = branch("main");
        assert_eq!(GitHub.name(), "GitHub");
        assert_eq!(
            GitHub.issue(&repo, 42),
            "https://github.com/acme/widgets/issues/42"
        );
        assert_eq!(
            GitHub.pull_request(&repo, 7),
            "https://github.com/acme/widgets/pull/7"
        );
        assert_eq!(
            GitHub.branch(&repo, &main),
            "https://github.com/acme/widgets/tree/main"
        );
        assert_eq!(
            GitHub.file(&repo, Revision::Branch(&main), "src/lib.rs"),
            "https://github.com/acme/widgets/blob/main/src/lib.rs"
        );
        assert_eq!(
            GitHub.directory(&repo, Revision::Branch(&main), "src"),
            "https://github.com/acme/widgets/tree/main/src"
        );
        assert_eq!(
            GitHub.commit(&repo, "a1b2c3d"),
            "https://github.com/acme/widgets/commit/a1b2c3d"
        );
    }

    #[test]
    fn a_branch_and_a_path_are_percent_encoded_keeping_slashes() {
        let repo = widgets();
        let odd = branch("alice/42-fix#2");
        assert_eq!(
            GitHub.branch(&repo, &odd),
            "https://github.com/acme/widgets/tree/alice/42-fix%232"
        );
        assert_eq!(
            GitHub.file(&repo, Revision::Branch(&odd), "docs/a b%.md"),
            "https://github.com/acme/widgets/blob/alice/42-fix%232/docs/a%20b%25.md"
        );
        assert_eq!(
            GitHub.file(&repo, Revision::DefaultBranch, "/docs/~a_b-c.d"),
            "https://github.com/acme/widgets/blob/HEAD/docs/~a_b-c.d"
        );
    }

    #[test]
    fn a_name_with_a_space_is_encoded_as_its_bytes() {
        // A branch name never holds a space, so a space in a branch is shown through the encoder
        // a branch and a path share: a path of a directory named like the branch.
        let repo = widgets();
        assert_eq!(
            GitHub.directory(&repo, Revision::DefaultBranch, "alice/42-fix #2"),
            "https://github.com/acme/widgets/tree/HEAD/alice/42-fix%20%232"
        );
    }

    #[test]
    fn a_non_ascii_path_is_encoded_as_its_utf8_bytes() {
        assert_eq!(
            GitHub.file(&widgets(), Revision::DefaultBranch, "docs/é/日.md"),
            "https://github.com/acme/widgets/blob/HEAD/docs/%C3%A9/%E6%97%A5.md"
        );
        assert_eq!(
            GitHub.branch(&widgets(), &branch("bob/Ünïcode")),
            "https://github.com/acme/widgets/tree/bob/%C3%9Cn%C3%AFcode"
        );
    }

    #[test]
    fn the_default_branch_is_head_and_an_empty_directory_is_the_root() {
        let repo = widgets();
        assert_eq!(
            GitHub.file(&repo, Revision::DefaultBranch, "README.md"),
            "https://github.com/acme/widgets/blob/HEAD/README.md"
        );
        assert_eq!(
            GitHub.directory(&repo, Revision::DefaultBranch, ""),
            "https://github.com/acme/widgets/tree/HEAD"
        );
        assert_eq!(
            GitHub.directory(&repo, Revision::DefaultBranch, "/"),
            "https://github.com/acme/widgets/tree/HEAD"
        );
        assert_eq!(
            GitHub.directory(&repo, Revision::Branch(&branch("main")), ""),
            "https://github.com/acme/widgets/tree/main"
        );
    }

    #[test]
    fn a_nested_namespace_is_kept_in_every_address() {
        let repo = RemoteUrl::parse("github.com/acme/tools/widgets").unwrap();
        assert_eq!(
            GitHub.pull_request(&repo, 3),
            "https://github.com/acme/tools/widgets/pull/3"
        );
    }

    #[test]
    fn the_standard_registry_knows_github_by_its_host_only() {
        let registry = ForgeRegistry::standard();
        let forge = registry.of(&widgets()).unwrap();
        assert_eq!(forge.name(), "GitHub");
        assert_eq!(
            forge.issue(&widgets(), 42),
            "https://github.com/acme/widgets/issues/42"
        );
        let elsewhere = RemoteUrl::parse("git@git.example.com:acme/widgets.git").unwrap();
        assert!(registry.of(&elsewhere).is_none());
        assert!(ForgeRegistry::empty().of(&widgets()).is_none());
    }

    #[test]
    fn another_host_is_registered_with_with() {
        let elsewhere = RemoteUrl::parse("https://git.example.com/acme/widgets").unwrap();
        let registry = ForgeRegistry::standard().with("Git.Example.com", Box::new(GitHub));
        let forge = registry.of(&elsewhere).unwrap();
        assert_eq!(
            forge.pull_request(&elsewhere, 9),
            "https://git.example.com/acme/widgets/pull/9"
        );
        assert!(registry.of(&widgets()).is_some());
    }
}
