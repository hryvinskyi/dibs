//! Repositories of a project: their ids and the remote each one is cloned from.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::text::{is_slug, string_value};

/// The id of one repository of a project (`api`, `web`): 1 to 32 characters, lowercase letters,
/// digits and `-`, starting with a letter or digit. It qualifies area names (`api:billing`) and
/// paths (`api:src/lib.rs`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RepoId(String);

/// Why a text is not a repository id.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RepoIdError {
    /// The text breaks the id's character rules.
    #[error(
        "a repository id is 1 to 32 characters: lowercase letters, digits and '-', starting with a letter or digit"
    )]
    Malformed,
}

impl RepoId {
    /// The id of the one repository of a project whose settings list no repositories.
    pub const IMPLICIT: &'static str = "default";

    /// Parses a repository id.
    ///
    /// # Errors
    /// [`RepoIdError::Malformed`] when the text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, RepoIdError> {
        if is_slug(value, 32, &['-']) {
            Ok(Self(value.to_owned()))
        } else {
            Err(RepoIdError::Malformed)
        }
    }

    /// The id of the one repository of a project whose settings list no repositories.
    pub fn implicit() -> Self {
        Self(Self::IMPLICIT.to_owned())
    }
}

string_value!(RepoId, RepoIdError);

/// A repository's remote in one normalised form, `<host>/<owner>/<name>`, lowercased, so the `https`
/// and `ssh` spellings of one repository compare equal. A namespace of several segments
/// (`<host>/<group>/<subgroup>/<name>`) is kept whole. User names, passwords, ports, a trailing
/// `.git` and a trailing `/` are dropped.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RemoteUrl(String);

/// Why a text is not a remote.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RemoteUrlError {
    /// The text is empty.
    #[error("a repository remote is empty")]
    Empty,
    /// The text names a URL scheme other than https, http, ssh, git or git+ssh.
    #[error("a repository remote uses https, http, ssh, git or git+ssh")]
    UnsupportedScheme,
    /// The text is no remote form this type knows.
    #[error(
        "a repository remote looks like 'https://host/owner/name', 'git@host:owner/name' or 'host/owner/name'"
    )]
    Malformed,
}

const URL_SCHEMES: &[&str] = &["https", "http", "ssh", "git", "git+ssh"];

impl RemoteUrl {
    /// Parses a remote from any of the forms `https://host/owner/name(.git)`,
    /// `ssh://user@host:port/owner/name`, `user@host:owner/name` (the scp-like form) and
    /// `host/owner/name`, and normalises it.
    ///
    /// # Errors
    /// [`RemoteUrlError`] naming what is wrong. A local path (`/srv/repo`, `C:\work\repo`,
    /// `c:/work/repo`, `file://…`) is refused.
    pub fn parse(value: &str) -> Result<Self, RemoteUrlError> {
        let value = value.trim();
        if value.is_empty() {
            return Err(RemoteUrlError::Empty);
        }
        let forbidden = |c: char| c.is_whitespace() || c.is_control() || "\\?#".contains(c);
        if value.chars().any(forbidden) {
            return Err(RemoteUrlError::Malformed);
        }
        let (host, path) = split_remote(value)?;
        let host = normalise_host(host)?;
        let path = normalise_remote_path(path)?;
        Ok(Self(format!("{host}/{path}")))
    }

    /// The host part (`example.com`).
    pub fn host(&self) -> &str {
        self.0.split('/').next().unwrap_or_default()
    }

    /// The path part after the host (`acme/api`).
    pub fn path(&self) -> &str {
        self.0
            .split_once('/')
            .map(|(_, path)| path)
            .unwrap_or_default()
    }
}

string_value!(RemoteUrl, RemoteUrlError);

/// Splits a remote into its host (possibly with user and port) and its path.
fn split_remote(value: &str) -> Result<(&str, &str), RemoteUrlError> {
    if let Some((scheme, rest)) = value.split_once("://") {
        if !URL_SCHEMES.contains(&scheme.to_ascii_lowercase().as_str()) {
            return Err(RemoteUrlError::UnsupportedScheme);
        }
        let (authority, path) = rest.split_once('/').ok_or(RemoteUrlError::Malformed)?;
        let host = strip_port(strip_user(authority))?;
        return Ok((host, path));
    }
    match value.split_once(':') {
        Some((left, path)) if !left.contains('/') => {
            let host = strip_user(left);
            let drive_letter = host.len() == 1 && host.chars().all(|c| c.is_ascii_alphabetic());
            if drive_letter || path.starts_with('/') {
                return Err(RemoteUrlError::Malformed);
            }
            Ok((host, path))
        }
        _ => value.split_once('/').ok_or(RemoteUrlError::Malformed),
    }
}

fn strip_user(authority: &str) -> &str {
    authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host)
}

fn strip_port(authority: &str) -> Result<&str, RemoteUrlError> {
    match authority.split_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            Ok(host)
        }
        Some(_) => Err(RemoteUrlError::Malformed),
        None => Ok(authority),
    }
}

fn normalise_host(host: &str) -> Result<String, RemoteUrlError> {
    let host = host.to_ascii_lowercase();
    let chars_ok = host
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-');
    let edges_ok = !host.starts_with(['.', '-']) && !host.ends_with(['.', '-']);
    if host.is_empty() || !chars_ok || !edges_ok {
        return Err(RemoteUrlError::Malformed);
    }
    Ok(host)
}

fn normalise_remote_path(path: &str) -> Result<String, RemoteUrlError> {
    let path = path.trim_end_matches('/');
    let lower = path.to_ascii_lowercase();
    let lower = lower.strip_suffix(".git").unwrap_or(&lower);
    let segments: Vec<&str> = lower.split('/').collect();
    let segment_ok = |s: &&str| {
        !s.is_empty()
            && *s != "."
            && *s != ".."
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    };
    if segments.len() < 2 || !segments.iter().all(segment_ok) {
        return Err(RemoteUrlError::Malformed);
    }
    Ok(segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_id_table() {
        let cases: &[(&str, bool)] = &[
            ("api", true),
            ("web-2", true),
            ("", false),
            ("Api", false),
            ("a:b", false),
            ("-api", false),
            (&"r".repeat(33), false),
        ];
        for (value, ok) in cases {
            assert_eq!(RepoId::parse(value).is_ok(), *ok, "{value:?}");
        }
        assert_eq!(RepoId::implicit().as_str(), "default");
    }

    #[test]
    fn every_spelling_of_one_remote_normalises_to_one_form() {
        let spellings = [
            "https://example.com/acme/api",
            "https://example.com/acme/api.git",
            "https://example.com/acme/api/",
            "https://user:secret@example.com/acme/api.git",
            "HTTPS://Example.COM/Acme/API.git",
            "http://example.com:8080/acme/api",
            "ssh://git@example.com/acme/api.git",
            "ssh://git@example.com:2222/acme/api",
            "git+ssh://git@example.com/acme/api",
            "git://example.com/acme/api",
            "git@example.com:acme/api.git",
            "git@example.com:Acme/Api",
            "example.com:acme/api",
            "example.com/acme/api",
            "  example.com/acme/api  ",
        ];
        for spelling in spellings {
            let remote = RemoteUrl::parse(spelling).unwrap();
            assert_eq!(remote.as_str(), "example.com/acme/api", "{spelling:?}");
            assert_eq!(remote.host(), "example.com");
            assert_eq!(remote.path(), "acme/api");
        }
    }

    #[test]
    fn nested_namespaces_are_kept() {
        let remote = RemoteUrl::parse("https://example.com/acme/tools/api.git").unwrap();
        assert_eq!(remote.as_str(), "example.com/acme/tools/api");
    }

    #[test]
    fn local_paths_and_malformed_remotes_are_refused() {
        let cases: &[(&str, RemoteUrlError)] = &[
            ("", RemoteUrlError::Empty),
            ("   ", RemoteUrlError::Empty),
            ("file:///srv/acme/api", RemoteUrlError::UnsupportedScheme),
            (
                "ftp://example.com/acme/api",
                RemoteUrlError::UnsupportedScheme,
            ),
            ("/srv/acme/api", RemoteUrlError::Malformed),
            ("C:\\work\\api", RemoteUrlError::Malformed),
            ("c:/work/api", RemoteUrlError::Malformed),
            ("c:work/api", RemoteUrlError::Malformed),
            ("git@example.com:/srv/api.git", RemoteUrlError::Malformed),
            ("example.com/api", RemoteUrlError::Malformed),
            ("example.com", RemoteUrlError::Malformed),
            ("https://example.com/", RemoteUrlError::Malformed),
            ("https://example.com/acme//api", RemoteUrlError::Malformed),
            ("https://example.com/acme/../api", RemoteUrlError::Malformed),
            (
                "https://example.com/acme/api?x=1",
                RemoteUrlError::Malformed,
            ),
            (
                "https://example.com/acme/api#main",
                RemoteUrlError::Malformed,
            ),
            (
                "https://example.com:http/acme/api",
                RemoteUrlError::Malformed,
            ),
            ("https://exa mple.com/acme/api", RemoteUrlError::Malformed),
            ("https://-example.com/acme/api", RemoteUrlError::Malformed),
            ("https://example.com/acme/a pi", RemoteUrlError::Malformed),
        ];
        for (value, expected) in cases {
            assert_eq!(RemoteUrl::parse(value).as_ref(), Err(expected), "{value:?}");
        }
    }
}
