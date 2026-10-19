//! Paths inside a repository, path globs, and whether two globs can match one path.

use std::fmt;

use globset::{GlobBuilder, GlobMatcher};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::repo::{RepoId, RepoIdError};
use crate::text::string_value;

const MAX_PATH_BYTES: usize = 4096;
const MAX_GLOB_CHARS: usize = 500;
const GLOB_META: &[char] = &['*', '?', '[', ']', '{', '}'];

/// A path relative to a repository's root in one normalised form: segments joined by `/`, no `.`
/// segment, no empty segment, no `..`, never absolute. Comparison is case-sensitive.
///
/// The type accepts only that form. A host path (`C:\work\api\src\a.rs`, `/home/alice/api/src`) is
/// made relative to its repository's root before it becomes a `RelPath`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RelPath(String);

/// Why a text is not a relative path.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RelPathError {
    /// The text is empty, or only `.` segments.
    #[error("a path is empty")]
    Empty,
    /// The text is longer than 4096 bytes.
    #[error("a path is longer than 4096 bytes")]
    TooLong,
    /// The text contains a NUL character.
    #[error("a path contains a NUL character")]
    Nul,
    /// The text contains `\`.
    #[error("a path uses '/' as its separator, never '\\'")]
    Backslash,
    /// The text starts at a root (`/`) or a drive (`C:`).
    #[error("a path is relative to its repository's root, never absolute")]
    Absolute,
    /// The text contains a `..` segment.
    #[error("a path never leaves its repository through '..'")]
    ParentSegment,
    /// The text contains an empty segment (`a//b`, a trailing `/`).
    #[error("a path has an empty segment")]
    EmptySegment,
}

impl RelPath {
    /// Parses and normalises a relative path: `./src/./lib.rs` becomes `src/lib.rs`.
    ///
    /// # Errors
    /// [`RelPathError`] naming the first rule the text breaks.
    pub fn parse(value: &str) -> Result<Self, RelPathError> {
        if value.is_empty() {
            return Err(RelPathError::Empty);
        }
        if value.len() > MAX_PATH_BYTES {
            return Err(RelPathError::TooLong);
        }
        if value.contains('\0') {
            return Err(RelPathError::Nul);
        }
        if value.contains('\\') {
            return Err(RelPathError::Backslash);
        }
        if value.starts_with('/') || starts_with_drive(value) {
            return Err(RelPathError::Absolute);
        }
        let mut segments = Vec::new();
        for segment in value.split('/') {
            match segment {
                "." => {}
                ".." => return Err(RelPathError::ParentSegment),
                "" => return Err(RelPathError::EmptySegment),
                other => segments.push(other),
            }
        }
        if segments.is_empty() {
            return Err(RelPathError::Empty);
        }
        Ok(Self(segments.join("/")))
    }
}

string_value!(RelPath, RelPathError);

fn starts_with_drive(value: &str) -> bool {
    let mut chars = value.chars();
    matches!((chars.next(), chars.next()), (Some(letter), Some(':')) if letter.is_ascii_alphabetic())
}

/// A path inside one repository of a project. Shown as `repo:path` (`api:src/lib.rs`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RepoPath {
    /// The repository.
    pub repo: RepoId,
    /// The path relative to that repository's root.
    pub path: RelPath,
}

/// Why a text is not a qualified repository path.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RepoPathError {
    /// The text has no `repo:` prefix.
    #[error("a repository path is written 'repo:path'")]
    Unqualified,
    /// The repository part is not a repository id.
    #[error(transparent)]
    Repo(#[from] RepoIdError),
    /// The path part is not a relative path.
    #[error(transparent)]
    Path(#[from] RelPathError),
}

impl RepoPath {
    /// Parses `repo:path`.
    ///
    /// # Errors
    /// [`RepoPathError`] when either part is invalid or the prefix is missing.
    pub fn parse(value: &str) -> Result<Self, RepoPathError> {
        let (repo, path) = value.split_once(':').ok_or(RepoPathError::Unqualified)?;
        Ok(Self {
            repo: RepoId::parse(repo)?,
            path: RelPath::parse(path)?,
        })
    }
}

impl fmt::Display for RepoPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.repo, self.path)
    }
}

impl TryFrom<String> for RepoPath {
    type Error = RepoPathError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<RepoPath> for String {
    fn from(value: RepoPath) -> String {
        value.to_string()
    }
}

/// A glob over relative paths. `*` and `?` never cross a `/`; `**` as a whole segment crosses any
/// number of them (`packages/billing/**`, `**/*.lock`); `[...]` and `{a,b}` work as in shells.
/// Matching is case-sensitive.
#[derive(Clone)]
pub struct PathGlob {
    text: String,
    matcher: GlobMatcher,
}

/// Why a text is not a path glob.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PathGlobError {
    /// The text is empty.
    #[error("a path glob is empty")]
    Empty,
    /// The text is longer than 500 characters.
    #[error("a path glob is longer than 500 characters")]
    TooLong,
    /// The text contains a NUL character.
    #[error("a path glob contains a NUL character")]
    Nul,
    /// The text contains `\`.
    #[error("a path glob uses '/' as its separator and has no '\\' escapes")]
    Backslash,
    /// The text starts at a root or a drive.
    #[error("a path glob is relative to its repository's root, never absolute")]
    Absolute,
    /// The text contains a `.` or `..` segment.
    #[error("a path glob has no '.' or '..' segment")]
    DotSegment,
    /// The text contains an empty segment (`a//b`, a trailing `/`).
    #[error("a path glob has an empty segment")]
    EmptySegment,
    /// The glob syntax is invalid (an unclosed `[` or `{`).
    #[error("a path glob is not valid glob syntax: {0}")]
    Syntax(String),
}

impl PathGlob {
    /// Parses and compiles a glob.
    ///
    /// # Errors
    /// [`PathGlobError`] naming the first rule the text breaks.
    pub fn parse(value: &str) -> Result<Self, PathGlobError> {
        if value.is_empty() {
            return Err(PathGlobError::Empty);
        }
        if value.chars().count() > MAX_GLOB_CHARS {
            return Err(PathGlobError::TooLong);
        }
        if value.contains('\0') {
            return Err(PathGlobError::Nul);
        }
        if value.contains('\\') {
            return Err(PathGlobError::Backslash);
        }
        if value.starts_with('/') || starts_with_drive(value) {
            return Err(PathGlobError::Absolute);
        }
        for segment in value.split('/') {
            match segment {
                "." | ".." => return Err(PathGlobError::DotSegment),
                "" => return Err(PathGlobError::EmptySegment),
                _ => {}
            }
        }
        let glob = GlobBuilder::new(value)
            .literal_separator(true)
            .backslash_escape(false)
            .build()
            .map_err(|error| PathGlobError::Syntax(error.kind().to_string()))?;
        Ok(Self {
            text: value.to_owned(),
            matcher: glob.compile_matcher(),
        })
    }

    /// The glob as written.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Whether the glob matches the path.
    pub fn is_match(&self, path: &RelPath) -> bool {
        self.matcher.is_match(path.as_str())
    }

    fn is_literal(&self) -> bool {
        !self.text.contains(GLOB_META)
    }

    /// The text before the first glob metacharacter. Every path the glob matches starts with it.
    fn literal_prefix(&self) -> &str {
        self.text
            .find(GLOB_META)
            .map_or(self.text.as_str(), |at| &self.text[..at])
    }

    /// The text after the last glob metacharacter. Every path the glob matches ends with it. A `/`
    /// right after a `**` segment is not part of it, since `**/` also matches nothing at all.
    fn literal_suffix(&self) -> &str {
        let Some(at) = self.text.rfind(GLOB_META) else {
            return &self.text;
        };
        let suffix = &self.text[at + 1..];
        let after_double_star = self.text[..=at].ends_with("**");
        match suffix.strip_prefix('/') {
            Some(rest) if after_double_star => rest,
            _ => suffix,
        }
    }
}

impl fmt::Debug for PathGlob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PathGlob").field(&self.text).finish()
    }
}

impl fmt::Display for PathGlob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl PartialEq for PathGlob {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
    }
}

impl Eq for PathGlob {}

/// Whether some path could match both globs.
///
/// The answer is conservative: `false` only when no path can match both, `true` whenever one might.
/// When either glob has no metacharacter it is a single path, and the answer is exact. Otherwise
/// two globs are disjoint when their literal prefixes diverge (`packages/billing/**` and
/// `packages/search/**`) or their literal suffixes do (`**/*.lock` and `**/*.json`); anything else —
/// character classes and alternations that might never meet included — answers `true`.
pub fn globs_may_overlap(a: &PathGlob, b: &PathGlob) -> bool {
    if a.is_literal() {
        return b.matcher.is_match(&a.text);
    }
    if b.is_literal() {
        return a.matcher.is_match(&b.text);
    }
    let (pa, pb) = (a.literal_prefix(), b.literal_prefix());
    if !(pa.starts_with(pb) || pb.starts_with(pa)) {
        return false;
    }
    let (sa, sb) = (a.literal_suffix(), b.literal_suffix());
    sa.ends_with(sb) || sb.ends_with(sa)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glob(text: &str) -> PathGlob {
        PathGlob::parse(text).unwrap()
    }

    fn path(text: &str) -> RelPath {
        RelPath::parse(text).unwrap()
    }

    #[test]
    fn rel_path_normalises() {
        let cases: &[(&str, &str)] = &[
            ("src/lib.rs", "src/lib.rs"),
            ("./src/lib.rs", "src/lib.rs"),
            ("src/./lib.rs", "src/lib.rs"),
            ("composer.lock", "composer.lock"),
            ("a b/c d.txt", "a b/c d.txt"),
            ("Src/Lib.rs", "Src/Lib.rs"),
            ("dir:with:colons/a", "dir:with:colons/a"),
        ];
        for (input, expected) in cases {
            assert_eq!(path(input).as_str(), *expected, "{input:?}");
        }
    }

    #[test]
    fn rel_path_refusals_including_host_paths() {
        let long = "a/".repeat(2049);
        let cases: &[(&str, RelPathError)] = &[
            ("", RelPathError::Empty),
            (".", RelPathError::Empty),
            ("./.", RelPathError::Empty),
            (long.as_str(), RelPathError::TooLong),
            ("a\u{0}b", RelPathError::Nul),
            ("/etc/passwd", RelPathError::Absolute),
            ("/", RelPathError::Absolute),
            ("C:/work/api/src/a.rs", RelPathError::Absolute),
            ("c:/work/api/src/a.rs", RelPathError::Absolute),
            ("C:", RelPathError::Absolute),
            ("C:\\work\\api\\src\\a.rs", RelPathError::Backslash),
            ("src\\a.rs", RelPathError::Backslash),
            ("\\\\server\\share\\a.rs", RelPathError::Backslash),
            ("../outside", RelPathError::ParentSegment),
            ("src/../../outside", RelPathError::ParentSegment),
            ("src//lib.rs", RelPathError::EmptySegment),
            ("src/", RelPathError::EmptySegment),
        ];
        for (input, expected) in cases {
            assert_eq!(RelPath::parse(input).as_ref(), Err(expected), "{input:?}");
        }
    }

    #[test]
    fn rel_path_is_case_sensitive() {
        assert_ne!(path("src/Lib.rs"), path("src/lib.rs"));
    }

    #[test]
    fn repo_path_round_trips_as_qualified_text() {
        let qualified = RepoPath::parse("api:src/lib.rs").unwrap();
        assert_eq!(qualified.repo.as_str(), "api");
        assert_eq!(qualified.path.as_str(), "src/lib.rs");
        assert_eq!(qualified.to_string(), "api:src/lib.rs");
        assert_eq!(
            RepoPath::parse("web:dir:x/a").unwrap().path.as_str(),
            "dir:x/a",
            "only the first ':' separates the repository"
        );
        assert!(
            matches!(
                RepoPath::parse("web:a:b"),
                Err(RepoPathError::Path(RelPathError::Absolute))
            ),
            "one letter and a colon read as a drive"
        );
        assert_eq!(
            RepoPath::parse("src/lib.rs"),
            Err(RepoPathError::Unqualified)
        );
        assert!(matches!(
            RepoPath::parse("Api:x"),
            Err(RepoPathError::Repo(_))
        ));
        assert!(matches!(
            RepoPath::parse("api:/x"),
            Err(RepoPathError::Path(_))
        ));
        assert!(matches!(
            RepoPath::parse("api:C:\\x"),
            Err(RepoPathError::Path(_))
        ));
    }

    #[test]
    fn glob_refusals() {
        type Check = fn(&PathGlobError) -> bool;
        let cases: &[(&str, Check)] = &[
            ("", |e| *e == PathGlobError::Empty),
            ("/src/**", |e| *e == PathGlobError::Absolute),
            ("C:/src/**", |e| *e == PathGlobError::Absolute),
            ("src\\**", |e| *e == PathGlobError::Backslash),
            ("a\u{0}", |e| *e == PathGlobError::Nul),
            ("./src/**", |e| *e == PathGlobError::DotSegment),
            ("src/../**", |e| *e == PathGlobError::DotSegment),
            ("src//**", |e| *e == PathGlobError::EmptySegment),
            ("src/", |e| *e == PathGlobError::EmptySegment),
            ("src/[ab", |e| matches!(e, PathGlobError::Syntax(_))),
            ("src/{a,b", |e| matches!(e, PathGlobError::Syntax(_))),
        ];
        for (input, check) in cases {
            let error = PathGlob::parse(input).unwrap_err();
            assert!(check(&error), "{input:?} gave {error:?}");
        }
        assert_eq!(
            PathGlob::parse(&"a".repeat(501)).unwrap_err(),
            PathGlobError::TooLong
        );
    }

    #[test]
    fn glob_matching_semantics() {
        let cases: &[(&str, &str, bool)] = &[
            ("packages/billing/**", "packages/billing/src/a.php", true),
            ("packages/billing/**", "packages/billing", false),
            ("packages/billing/**", "packages/billing-x/a", false),
            ("*.lock", "composer.lock", true),
            ("*.lock", "sub/composer.lock", false),
            ("**/*.lock", "composer.lock", true),
            ("**/*.lock", "a/b/Cargo.lock", true),
            ("src/*.rs", "src/a/b.rs", false),
            ("src/[ab].rs", "src/a.rs", true),
            ("src/[ab].rs", "src/c.rs", false),
            ("{api,web}/**", "web/x", true),
            ("README.md", "readme.md", false),
        ];
        for (g, p, expected) in cases {
            assert_eq!(glob(g).is_match(&path(p)), *expected, "{g} ~ {p}");
        }
    }

    #[test]
    fn glob_overlap_table() {
        let cases: &[(&str, &str, bool, &str)] = &[
            (
                "packages/billing/**",
                "packages/search/**",
                false,
                "disjoint package dirs",
            ),
            (
                "packages/billing/**",
                "packages/billing-x/**",
                false,
                "sibling with a shared stem",
            ),
            ("packages/**", "packages/billing/**", true, "nested dirs"),
            (
                "packages/billing/**",
                "packages/billing/src/**",
                true,
                "nested dirs, deeper",
            ),
            (
                "packages/bill*/**",
                "packages/billing/**",
                true,
                "a star prefix of a dir",
            ),
            ("**", "composer.lock", true, "everything vs a file"),
            ("**", "packages/billing/**", true, "everything vs a dir"),
            (
                "packages/billing/**",
                "packages/billing/**",
                true,
                "identical",
            ),
            ("composer.lock", "composer.lock", true, "identical files"),
            ("composer.lock", "composer.json", false, "different files"),
            (
                "**/*.lock",
                "composer.lock",
                true,
                "extension glob vs a root file",
            ),
            (
                "**/*.lock",
                "packages/billing/composer.lock",
                true,
                "extension glob vs a deep file",
            ),
            (
                "**/*.lock",
                "composer.json",
                false,
                "extension glob vs another extension",
            ),
            ("**/*.lock", "**/*.json", false, "two extension globs"),
            (
                "*.lock",
                "packages/composer.lock",
                false,
                "a star never crosses a slash",
            ),
            (
                "**/*.lock",
                "packages/billing/**",
                true,
                "extension glob vs a dir",
            ),
            (
                "assets/*.css",
                "assets/*.js",
                false,
                "same dir, different extensions",
            ),
            ("src/[ab].rs", "src/a.rs", true, "class vs a member file"),
            (
                "src/[ab].rs",
                "src/c.rs",
                false,
                "class vs a non-member file",
            ),
            ("src/[ab]*.rs", "src/[bc]*.rs", true, "intersecting classes"),
            (
                "src/[ab]*.rs",
                "src/[cd]*.rs",
                true,
                "disjoint classes are not proven apart",
            ),
            (
                "src/[ab]*.rs",
                "lib/[ab]*.rs",
                false,
                "classes under different dirs",
            ),
            (
                "{api,web}/**",
                "web/src/**",
                true,
                "alternation vs a branch",
            ),
            (
                "**/schema/**",
                "packages/billing/schema/a.json",
                true,
                "double star in the middle",
            ),
            ("a/**/b", "a/b", true, "double star matching no segment"),
            ("**/x", "x", true, "leading double star matching no segment"),
            (
                "**/x",
                "**/y",
                false,
                "leading double stars, different names",
            ),
        ];
        for (a, b, expected, why) in cases {
            let (ga, gb) = (glob(a), glob(b));
            assert_eq!(globs_may_overlap(&ga, &gb), *expected, "{why}: {a} vs {b}");
            assert_eq!(
                globs_may_overlap(&gb, &ga),
                *expected,
                "{why} (reversed): {b} vs {a}"
            );
        }
    }

    #[test]
    fn overlap_never_misses_a_shared_path() {
        let globs = [
            "**",
            "**/*.lock",
            "*.lock",
            "packages/**",
            "packages/billing/**",
            "packages/*/src/**",
            "packages/bill*/**",
            "src/[ab]*.rs",
            "src/*.rs",
            "{api,web}/**",
            "**/x",
            "a/**/b",
            "composer.lock",
        ];
        let paths = [
            "composer.lock",
            "packages/billing/composer.lock",
            "packages/billing/src/a.php",
            "packages/search/src/a.php",
            "src/a.rs",
            "src/b1.rs",
            "web/x",
            "x",
            "a/b",
            "a/c/b",
        ];
        for a in globs {
            for b in globs {
                let (ga, gb) = (glob(a), glob(b));
                let shared = paths
                    .iter()
                    .any(|p| ga.is_match(&path(p)) && gb.is_match(&path(p)));
                if shared {
                    assert!(globs_may_overlap(&ga, &gb), "{a} and {b} share a path");
                }
            }
        }
    }
}
