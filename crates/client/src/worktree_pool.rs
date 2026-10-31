//! A checkout's pool of worktrees as this machine keeps it: the settings that declare the pool, and
//! the mark of each worktree of it that was prepared.
//!
//! Both live in the repository's git directories, never in a work tree. The settings are the
//! `[worktrees]` table of `dibs/worktrees.toml` in the repository's common directory (`.git/` of an
//! ordinary clone): where the worktrees live, how many there are and which commands of the project
//! prepare, reset and stop one. Those commands run on the machine of whoever reads the file, so the
//! file is one no branch can carry — git never tracks, pulls or checks out what lies in its own
//! directory — and a command in it is only ever the member's own. A worktree of the pool is marked
//! prepared by `dibs/pool-worktree.toml` in its own git directory (`.git/worktrees/<id>/`), whose
//! `[pool_worktree]` table names the checkout that lends it and its name, so nothing dibs writes
//! about a worktree lies in its tree.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::Duration;

use dibs_domain::settings::parse_duration;
use dibs_domain::worktree::{PoolId, PoolIdError, WorktreeName};

use crate::checkout::{CheckoutError, lexical, resolve_existing};
use crate::git_dirs::GitDirs;

/// Where a repository's pool settings live, from its common directory.
pub const SETTINGS_FILE: &str = "dibs/worktrees.toml";

/// Where a pool worktree's mark lives, from its own git directory.
pub const MARKER_FILE: &str = "dibs/pool-worktree.toml";

/// The pool of worktrees a checkout lends: `<prefix>1` to `<prefix><count>` under `root`, and the
/// project's commands that prepare a missing one, reset a reused one and stop the environment of
/// one left idle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeSettings {
    /// The directory the worktrees live in, absolute: by default beside the checkout,
    /// `<checkout>-worktrees`.
    pub root: PathBuf,
    /// How many worktrees there are, 1 to [`WorktreeSettings::MAX_COUNT`].
    pub count: u8,
    /// What each worktree's name starts with; `worktree` by default.
    pub prefix: String,
    /// What a worktree is reset to; `origin/main` by default.
    pub base: String,
    /// The command that makes a worktree missing on disk ready to work in.
    pub prepare: String,
    /// The command that brings a reused worktree back to the base.
    pub reset: String,
    /// The command that stops the environment of a worktree left free, when the project has one.
    pub idle: Option<String>,
    /// How long a worktree stays free before its environment is stopped; 30 minutes by default.
    pub idle_after: Duration,
}

impl WorktreeSettings {
    /// The most worktrees one pool has.
    pub const MAX_COUNT: u8 = 32;

    /// The names of the worktrees, in order: `<prefix>1` to `<prefix><count>`.
    pub fn names(&self) -> Vec<WorktreeName> {
        (1..=self.count)
            .filter_map(|n| WorktreeName::parse(&format!("{}{n}", self.prefix)).ok())
            .collect()
    }

    /// The directory of the worktree `name`.
    pub fn path_of(&self, name: &WorktreeName) -> PathBuf {
        self.root.join(name.as_str())
    }
}

/// What the mark of a pool worktree says about it: the checkout that lends it and its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolWorktree {
    /// The checkout whose pool the worktree belongs to, absolute.
    pub checkout: PathBuf,
    /// The worktree's name in that pool.
    pub name: WorktreeName,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsFile {
    worktrees: Option<WorktreesTable>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorktreesTable {
    root: Option<String>,
    count: Option<i64>,
    prefix: Option<String>,
    base: Option<String>,
    prepare: Option<String>,
    reset: Option<String>,
    idle: Option<String>,
    idle_after: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MarkerFile {
    pool_worktree: MarkerTable,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MarkerTable {
    checkout: String,
    name: String,
}

/// The file of the pool settings of the repository `dirs` is a work tree of.
pub fn settings_file(dirs: &GitDirs) -> PathBuf {
    dirs.common_dir().join(SETTINGS_FILE)
}

/// The pool settings of the repository `dirs` is a work tree of, for the checkout whose main work
/// tree is `checkout`; `None` when the file does not exist or holds no `[worktrees]` table.
///
/// # Errors
/// [`CheckoutError`] naming the file when it cannot be read or is not valid.
pub fn read_settings(
    dirs: &GitDirs,
    checkout: &Path,
) -> Result<Option<WorktreeSettings>, CheckoutError> {
    let path = settings_file(dirs);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(CheckoutError::Unreadable { path, source }),
    };
    parse_settings(&path, &text, checkout)
}

/// Parses the pool settings at `path`, of the checkout whose main work tree is `checkout`: a
/// relative `root` and the default one are taken from the checkout.
///
/// # Errors
/// [`CheckoutError::Invalid`] naming the file and the first rule broken.
pub fn parse_settings(
    path: &Path,
    text: &str,
    checkout: &Path,
) -> Result<Option<WorktreeSettings>, CheckoutError> {
    let invalid = |reason: String| CheckoutError::Invalid {
        path: path.to_path_buf(),
        reason,
    };
    let file: SettingsFile =
        toml::from_str(text).map_err(|error| invalid(error.message().to_owned()))?;
    file.worktrees
        .map(|table| worktree_settings(checkout, table))
        .transpose()
        .map_err(invalid)
}

/// The `[worktrees]` table, its defaults filled in for the checkout at `directory`.
fn worktree_settings(directory: &Path, table: WorktreesTable) -> Result<WorktreeSettings, String> {
    let count = table
        .count
        .ok_or("worktrees: count is required: how many worktrees the pool has")?;
    let count = u8::try_from(count)
        .ok()
        .filter(|count| (1..=WorktreeSettings::MAX_COUNT).contains(count))
        .ok_or_else(|| format!("worktrees: count is 1 to {}", WorktreeSettings::MAX_COUNT))?;
    let prefix = table.prefix.unwrap_or_else(|| "worktree".to_owned());
    if WorktreeName::parse(&format!("{prefix}{count}")).is_err() {
        return Err(format!(
            "worktrees: prefix '{prefix}' does not make worktree names: a name is 1 to 64 lowercase \
             letters, digits, '.', '_' or '-', starting with a letter or a digit"
        ));
    }
    let root = match table.root {
        Some(root) if root.trim().is_empty() => {
            return Err("worktrees: root is a directory".to_owned());
        }
        Some(root) => lexical(&directory.join(root)),
        None => default_root(directory)?,
    };
    if root.starts_with(lexical(directory)) {
        return Err(format!(
            "worktrees: root {} is the checkout or lies inside it; the worktrees live outside it",
            root.display()
        ));
    }
    let base = table.base.unwrap_or_else(|| "origin/main".to_owned());
    if base.is_empty() || base.starts_with('-') || base.chars().any(char::is_whitespace) {
        return Err(format!(
            "worktrees: base '{base}' is no ref a worktree can be reset to"
        ));
    }
    let command = |name: &str, text: Option<String>| -> Result<Option<String>, String> {
        match text {
            Some(text) if text.trim().is_empty() => {
                Err(format!("worktrees: {name} is a command, not empty"))
            }
            other => Ok(other),
        }
    };
    let prepare = command("prepare", table.prepare)?.ok_or(
        "worktrees: prepare is required: the command that makes a worktree ready to work in",
    )?;
    let reset = command("reset", table.reset)?.ok_or(
        "worktrees: reset is required: the command that brings a reused worktree back to the base",
    )?;
    let idle = command("idle", table.idle)?;
    let idle_after = match table.idle_after {
        Some(text) => parse_duration(&text)
            .ok()
            .filter(|duration| duration.is_positive())
            .ok_or_else(|| format!("worktrees: idle_after '{text}' is no duration like '30m'"))?,
        None => Duration::minutes(30),
    };
    Ok(WorktreeSettings {
        root,
        count,
        prefix,
        base,
        prepare,
        reset,
        idle,
        idle_after,
    })
}

/// `<checkout>-worktrees`, beside the checkout.
fn default_root(directory: &Path) -> Result<PathBuf, String> {
    let directory = lexical(directory);
    let name = directory
        .file_name()
        .ok_or("worktrees: the checkout has no directory name to put its pool beside; set root")?;
    let mut sibling = name.to_os_string();
    sibling.push("-worktrees");
    Ok(directory.with_file_name(sibling))
}

/// The file of the mark of the work tree `dirs` describes.
pub fn marker_file(dirs: &GitDirs) -> PathBuf {
    dirs.git_dir().join(MARKER_FILE)
}

/// The mark of the work tree `dirs` describes; `None` when it has none, or one that cannot be read
/// as a mark.
pub fn read_marker(dirs: &GitDirs) -> Option<PoolWorktree> {
    let text = fs::read_to_string(marker_file(dirs)).ok()?;
    let file: MarkerFile = toml::from_str(&text).ok()?;
    let checkout = PathBuf::from(file.pool_worktree.checkout);
    if !checkout.is_absolute() {
        return None;
    }
    let name = WorktreeName::parse(&file.pool_worktree.name).ok()?;
    Some(PoolWorktree { checkout, name })
}

/// Marks the work tree `dirs` describes as the prepared worktree `mark` names. Written beside the
/// file into one made afresh, whatever lay at that name removed first, then renamed over it.
///
/// # Errors
/// The file system's failure.
pub fn write_marker(dirs: &GitDirs, mark: &PoolWorktree) -> io::Result<()> {
    let path = marker_file(dirs);
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    fs::create_dir_all(dir)?;
    let file = MarkerFile {
        pool_worktree: MarkerTable {
            checkout: mark.checkout.to_string_lossy().into_owned(),
            name: mark.name.to_string(),
        },
    };
    let text = toml::to_string(&file).map_err(io::Error::other)?;
    let mut temp = tempfile::Builder::new().prefix(".dibs-").tempfile_in(dir)?;
    temp.write_all(text.as_bytes())?;
    temp.persist(&path).map_err(|error| error.error)?;
    Ok(())
}

/// The id of the pool whose worktrees live in `root`: a digest of the directory as the file
/// system resolves it, as far as it exists, so every spelling of one directory — through a link,
/// with `..` — names one pool, and two checkouts of one project on one machine name two.
///
/// # Errors
/// None in practice: the digest is always written as a valid id.
pub fn pool_id(root: &Path) -> Result<PoolId, PoolIdError> {
    let lexical_root = lexical(root);
    let resolved = resolve_existing(&lexical_root).unwrap_or(lexical_root);
    let digest = Sha256::digest(resolved.to_string_lossy().as_bytes());
    let hex: String = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    PoolId::parse(&hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    const POOL: &str = "[worktrees]\ncount = 3\nprepare = \"/opt/kit/prepare.sh\"\nreset = \"/opt/kit/reset.sh\"\n";

    fn parse(text: &str) -> Result<Option<WorktreeSettings>, CheckoutError> {
        parse_settings(
            Path::new("/home/alice/widgets/.git/dibs/worktrees.toml"),
            text,
            Path::new("/home/alice/widgets"),
        )
    }

    #[test]
    fn a_worktrees_table_fills_in_its_defaults_beside_the_checkout() {
        let pool = parse(POOL).unwrap().unwrap();
        assert_eq!(pool.root, PathBuf::from("/home/alice/widgets-worktrees"));
        assert_eq!(pool.count, 3);
        assert_eq!(pool.base, "origin/main");
        assert_eq!(pool.prepare, "/opt/kit/prepare.sh");
        assert_eq!(pool.reset, "/opt/kit/reset.sh");
        assert_eq!(pool.idle, None);
        assert_eq!(pool.idle_after, Duration::minutes(30));
        let names: Vec<String> = pool.names().iter().map(ToString::to_string).collect();
        assert_eq!(names, ["worktree1", "worktree2", "worktree3"]);
        assert_eq!(
            pool.path_of(&pool.names()[1]),
            PathBuf::from("/home/alice/widgets-worktrees/worktree2")
        );

        let custom = format!(
            "{POOL}root = \"../pools/widgets\"\nprefix = \"wt-\"\nbase = \"main\"\nidle = \"stop.sh\"\nidle_after = \"1h\"\n"
        );
        let pool = parse(&custom).unwrap().unwrap();
        assert_eq!(
            pool.root,
            PathBuf::from("/home/alice/pools/widgets"),
            "a relative root is taken from the checkout, not from the git directory"
        );
        assert_eq!(pool.names()[0].as_str(), "wt-1");
        assert_eq!(pool.base, "main");
        assert_eq!(pool.idle.as_deref(), Some("stop.sh"));
        assert_eq!(pool.idle_after, Duration::hours(1));
        assert_eq!(parse("").unwrap(), None, "a file without the table");
    }

    #[test]
    fn a_broken_rule_or_an_unknown_key_is_refused_naming_the_file() {
        for (text, needle) in [
            (format!("{POOL}size = 4\n"), "size"),
            (
                format!("server = \"https://dibs.example.com\"\n{POOL}"),
                "server",
            ),
            (POOL.replace("count = 3", "count = 0"), "count is 1 to 32"),
            (POOL.replace("count = 3", "count = 33"), "count is 1 to 32"),
            (POOL.replace("count = 3\n", ""), "count is required"),
            (
                POOL.replace("reset = \"/opt/kit/reset.sh\"\n", ""),
                "reset is required",
            ),
            (format!("{POOL}prefix = \"Work Tree\"\n"), "prefix"),
            (format!("{POOL}root = \"pool\"\n"), "inside it"),
            (format!("{POOL}base = \"--force\"\n"), "base"),
            (format!("{POOL}idle_after = \"soon\"\n"), "idle_after"),
            (
                POOL.replace("\"/opt/kit/prepare.sh\"", "\" \""),
                "prepare is a command",
            ),
        ] {
            let error = parse(&text).unwrap_err().to_string();
            assert!(error.contains("worktrees.toml"), "{error}");
            assert!(error.contains(needle), "{text:?}: {error}");
        }
    }

    fn git_dirs(dir: &Path) -> GitDirs {
        fs::create_dir_all(dir.join(".git")).unwrap();
        GitDirs::at(dir).unwrap()
    }

    #[test]
    fn the_settings_are_read_from_the_common_directory_and_absent_ones_are_no_pool() {
        let dir = tempfile::tempdir().unwrap();
        let checkout = dir.path().join("widgets");
        let dirs = git_dirs(&checkout);
        assert_eq!(read_settings(&dirs, &checkout).unwrap(), None);
        assert_eq!(
            settings_file(&dirs),
            checkout.join(".git/dibs/worktrees.toml")
        );
        fs::create_dir_all(checkout.join(".git/dibs")).unwrap();
        fs::write(settings_file(&dirs), POOL).unwrap();
        let pool = read_settings(&dirs, &checkout).unwrap().unwrap();
        assert_eq!(pool.root, dir.path().join("widgets-worktrees"));
    }

    #[test]
    fn a_mark_is_written_into_the_git_directory_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("worktree2");
        let dirs = git_dirs(&worktree);
        assert_eq!(read_marker(&dirs), None);
        let mark = PoolWorktree {
            checkout: PathBuf::from("/home/alice/widgets"),
            name: WorktreeName::parse("worktree2").unwrap(),
        };
        write_marker(&dirs, &mark).unwrap();
        write_marker(&dirs, &mark).unwrap();
        assert_eq!(read_marker(&dirs), Some(mark));
        assert!(worktree.join(".git/dibs/pool-worktree.toml").is_file());
        let text = fs::read_to_string(marker_file(&dirs)).unwrap();
        assert!(text.contains("[pool_worktree]"), "{text}");
        fs::write(
            marker_file(&dirs),
            "[pool_worktree]\ncheckout = \"widgets\"\nname = \"worktree2\"\n",
        )
        .unwrap();
        assert_eq!(
            read_marker(&dirs),
            None,
            "a relative checkout marks nothing"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_pool_is_named_by_its_directory_however_it_is_spelled() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("widgets-worktrees");
        let before = pool_id(&root).unwrap();
        fs::create_dir_all(&root).unwrap();
        assert_eq!(
            pool_id(&root).unwrap(),
            before,
            "made or not, one directory"
        );
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        assert_eq!(pool_id(&link).unwrap(), before);
        assert_eq!(
            pool_id(&dir.path().join("other/../widgets-worktrees")).unwrap(),
            before
        );
        assert_ne!(
            pool_id(&dir.path().join("gadgets-worktrees")).unwrap(),
            before
        );
        assert!(pool_id(&root).unwrap().as_str().len() <= PoolId::MAX_CHARS);
    }
}
