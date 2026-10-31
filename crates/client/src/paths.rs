//! Paths in the spelling git, the project's commands and a person all read the same way: the one
//! dibs hands to git, writes into a file, prints and compares.
//!
//! On Windows the file system answers a path it resolves in its verbatim form (`\\?\C:\…`,
//! `\\?\UNC\server\share\…`), which git takes for a network path and most programs do not read.
//! Such a path is written back in its ordinary spelling (`C:\…`, `\\server\share\…`) whenever that
//! spelling names the same file — no part of it is a device name such as `con`, ends in a dot or a
//! space, holds a `/`, or is `.` or `..`, which the ordinary spelling would read differently — and
//! every separator is written as `\`. A verbatim path with no ordinary spelling, such as a volume
//! named by its id, is left as it is. Elsewhere a path is left exactly as it is.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// `path` in its ordinary spelling: on Windows without a verbatim prefix whenever it can be named
/// without one, its separators written as `\`; elsewhere unchanged.
#[cfg(windows)]
pub fn plain(path: &Path) -> PathBuf {
    windows::plain(path)
}

/// `path` in its ordinary spelling: on Windows without a verbatim prefix whenever it can be named
/// without one, its separators written as `\`; elsewhere unchanged.
#[cfg(not(windows))]
pub fn plain(path: &Path) -> PathBuf {
    path.to_path_buf()
}

/// `path` as the file system resolves it — absolute, every link followed — in its ordinary
/// spelling.
///
/// # Errors
/// The file system's failure, as when `path` does not exist.
pub fn canonical(path: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(path).map(|resolved| plain(&resolved))
}

/// `path` resolved by the file system as far as it exists, with the rest appended as written, in
/// its ordinary spelling; `None` when no part of it exists or a part that does not ends in `..`.
pub fn resolve_existing(path: &Path) -> Option<PathBuf> {
    let mut rest = Vec::new();
    let mut existing = path;
    loop {
        if let Ok(resolved) = canonical(existing) {
            let mut out = resolved;
            out.extend(rest.iter().rev());
            return Some(out);
        }
        rest.push(existing.file_name()?.to_owned());
        existing = existing.parent()?;
    }
}

/// [`resolve_existing`], or `path` in its ordinary spelling when it cannot be resolved, so that a
/// file not made yet resolves like the directory it goes into.
pub fn resolved(path: &Path) -> PathBuf {
    resolve_existing(path).unwrap_or_else(|| plain(path))
}

/// Whether two paths name one directory as the file system resolves them; never when either does
/// not exist.
pub fn same_dir(a: &Path, b: &Path) -> bool {
    match (canonical(a), canonical(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[cfg(windows)]
mod windows {
    use std::ffi::OsStr;
    use std::path::{Component, Components, Path, PathBuf, Prefix};

    /// See [`super::plain`].
    pub(super) fn plain(path: &Path) -> PathBuf {
        let mut components = path.components();
        let Some(Component::Prefix(prefix)) = components.clone().next() else {
            return components.collect();
        };
        components.next();
        let head = match prefix.kind() {
            Prefix::Disk(drive) => Some(drive_head(drive)),
            Prefix::UNC(server, share) => share_head(server, share),
            Prefix::VerbatimDisk(drive) => {
                reads_alike(components.clone()).then(|| drive_head(drive))
            }
            Prefix::VerbatimUNC(server, share) => {
                if reads_alike(components.clone()) {
                    share_head(server, share)
                } else {
                    None
                }
            }
            Prefix::Verbatim(_) | Prefix::DeviceNS(_) => None,
        };
        let Some(head) = head else {
            return path.to_path_buf();
        };
        let mut out = PathBuf::from(head);
        out.extend(components);
        out
    }

    /// `C:`.
    fn drive_head(drive: u8) -> String {
        format!("{}:", char::from(drive))
    }

    /// `\\server\share`; `None` when either is not text or the share is not named.
    fn share_head(server: &OsStr, share: &OsStr) -> Option<String> {
        let server = server.to_str().filter(|server| !server.is_empty())?;
        let share = share.to_str().filter(|share| !share.is_empty())?;
        Some(format!(r"\\{server}\{share}"))
    }

    /// Whether the components after a verbatim prefix read the same once the prefix is ordinary:
    /// a root, then only names the ordinary spelling takes literally.
    fn reads_alike(mut rest: Components<'_>) -> bool {
        if rest.next() != Some(Component::RootDir) {
            return false;
        }
        rest.all(|component| match component {
            Component::Normal(name) => literal_name(name),
            _ => false,
        })
    }

    /// Whether `name` means itself in an ordinary path: it holds no `/`, ends in no dot or space,
    /// and is no device name, with or without an extension.
    fn literal_name(name: &OsStr) -> bool {
        let name = name.to_string_lossy();
        if name.is_empty() || name.contains('/') || name.ends_with(['.', ' ']) {
            return false;
        }
        let stem = name
            .split('.')
            .next()
            .unwrap_or_default()
            .trim_end_matches(' ');
        !is_device(stem)
    }

    /// Whether `stem` names a device wherever it stands in an ordinary path.
    fn is_device(stem: &str) -> bool {
        let upper = stem.to_ascii_uppercase();
        if matches!(
            upper.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) {
            return true;
        }
        ["COM", "LPT"].iter().any(|port| {
            upper.strip_prefix(port).is_some_and(|number| {
                let mut chars = number.chars();
                matches!(
                    (chars.next(), chars.next()),
                    (Some('0'..='9' | '¹' | '²' | '³'), None)
                )
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resolved_path_carries_no_verbatim_prefix_and_names_the_same_directory() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = canonical(dir.path()).unwrap();
        assert!(resolved.is_absolute());
        assert!(
            !resolved.to_string_lossy().starts_with(r"\\?\"),
            "{}",
            resolved.display()
        );
        assert_eq!(plain(&resolved), resolved, "already ordinary");
        assert!(same_dir(&resolved, dir.path()));
        fs::create_dir(dir.path().join("a")).unwrap();
        assert!(same_dir(&dir.path().join("a/.."), dir.path()));
        assert!(!same_dir(&dir.path().join("missing"), dir.path()));
        assert!(canonical(&dir.path().join("missing")).is_err());
    }

    #[test]
    fn a_path_not_made_yet_resolves_like_the_directory_it_goes_into() {
        let dir = tempfile::tempdir().unwrap();
        let resolved_dir = canonical(dir.path()).unwrap();
        assert_eq!(
            resolved(&dir.path().join("worktree1/src/new.rs")),
            resolved_dir.join("worktree1/src/new.rs")
        );
        assert_eq!(resolved(dir.path()), resolved_dir);
        assert_eq!(
            resolve_existing(&dir.path().join("worktree1")),
            Some(resolved_dir.join("worktree1"))
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn elsewhere_than_windows_a_path_is_left_exactly_as_it_is() {
        for text in [
            "/home/alice/widgets",
            "/home/alice//widgets/./tool/../",
            "widgets/tool",
            r"\\?\C:\Users\alice",
            "",
        ] {
            assert_eq!(plain(Path::new(text)).as_os_str(), text);
        }
    }

    #[cfg(windows)]
    fn plain_text(text: &str) -> String {
        plain(Path::new(text)).to_string_lossy().into_owned()
    }

    #[cfg(windows)]
    #[test]
    fn a_verbatim_drive_or_share_path_is_written_in_its_ordinary_spelling() {
        assert_eq!(
            plain_text(r"\\?\C:\Users\alice\widgets"),
            r"C:\Users\alice\widgets"
        );
        assert!(
            plain_text(r"\\?\c:\widgets").eq_ignore_ascii_case(r"c:\widgets"),
            "a drive letter is a drive letter in either case"
        );
        assert_eq!(plain_text(r"\\?\C:\"), r"C:\");
        assert_eq!(
            plain_text(r"\\?\UNC\server\share\widgets\.git"),
            r"\\server\share\widgets\.git"
        );
        assert_eq!(
            plain_text(r"\\?\C:\Users\alice\widgets.git\info"),
            r"C:\Users\alice\widgets.git\info"
        );
    }

    #[cfg(windows)]
    #[test]
    fn separators_are_written_as_backslashes() {
        assert_eq!(
            plain_text(r"C:/Users/alice/widgets\tool/worktrees\agent-a1"),
            r"C:\Users\alice\widgets\tool\worktrees\agent-a1"
        );
        assert_eq!(
            plain_text(r"\\server\share/widgets"),
            r"\\server\share\widgets"
        );
        assert_eq!(plain_text("widgets/tool"), r"widgets\tool");
        assert_eq!(plain_text("C:widgets/tool"), r"C:widgets\tool");
        assert_eq!(plain_text("/widgets/tool"), r"\widgets\tool");
        assert_eq!(plain_text(r"C:\widgets\"), r"C:\widgets");
    }

    #[cfg(windows)]
    #[test]
    fn a_verbatim_path_the_ordinary_spelling_would_read_differently_is_kept() {
        for text in [
            r"\\?\C:\Users\con",
            r"\\?\C:\Users\aux.txt",
            r"\\?\C:\Users\COM1",
            r"\\?\C:\Users\lpt9.log",
            r"\\?\C:\Users\dir.",
            r"\\?\C:\Users\dir ",
            r"\\?\C:\Users\a/b",
            r"\\?\C:\Users\..\alice",
            r"\\?\C:\Users\.\alice",
            r"\\?\C:",
            r"\\?\UNC\server",
            r"\\?\Volume{0b3b1e8a-1111-4d4d-9a9a-000000000000}\widgets",
            r"\\.\COM1",
        ] {
            assert_eq!(plain_text(text), text);
        }
        assert_eq!(
            plain_text(r"\\?\C:\Users\console\common"),
            r"C:\Users\console\common",
            "a name that only starts like a device is a name"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_plain_path_compares_equal_to_its_spellings_with_either_separator() {
        assert_eq!(
            plain(Path::new(
                r"\\?\C:\Users\alice\widgets\tool\worktrees\agent-a1"
            )),
            Path::new("C:/Users/alice/widgets/tool/worktrees/agent-a1")
        );
        assert_ne!(
            Path::new(r"\\?\C:\Users\alice\widgets"),
            Path::new(r"C:\Users\alice\widgets"),
            "a verbatim path never compares equal to its ordinary spelling"
        );
    }
}
