//! Putting a new binary in place of the installed one, in the installed one's directory.
//!
//! The new binary is written beside the installed one under a temporary name, made executable, and
//! then put in place in one step, so the name never points at half a binary. On macOS and Linux
//! that step is a rename over the installed file, which a running process survives. Windows does
//! not let a running executable be replaced, but lets it be renamed: there the installed one is
//! renamed aside to `<name>.old.exe`, the new one renamed into its place, and the old one deleted
//! by the next start ([`remove_previous`]). When the second rename fails, the first is undone.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::update::UpdateError;

/// The prefix of the temporary file the new binary is written to.
const TEMP_PREFIX: &str = ".dibs-update-";

/// The directory `binary` lies in.
///
/// # Errors
/// [`UpdateError::Replace`] when it has none.
pub fn directory_of(binary: &Path) -> Result<PathBuf, UpdateError> {
    binary
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .ok_or_else(|| UpdateError::Replace {
            dir: binary.to_path_buf(),
        })
}

/// Whether this user can create a file in `dir`.
///
/// # Errors
/// [`UpdateError::Unwritable`] when it cannot.
pub fn check_writable(dir: &Path) -> Result<(), UpdateError> {
    tempfile::Builder::new()
        .prefix(TEMP_PREFIX)
        .tempfile_in(dir)
        .map(drop)
        .map_err(|error| refused(dir, &error))
}

/// Replaces `binary` with `bytes`, made executable.
///
/// # Errors
/// [`UpdateError::Unwritable`] when its directory cannot be written; [`UpdateError::Replace`] when
/// the new binary cannot be put in place, in which case the installed one is kept.
pub fn replace(binary: &Path, bytes: &[u8]) -> Result<(), UpdateError> {
    let dir = directory_of(binary)?;
    let mut temp = tempfile::Builder::new()
        .prefix(TEMP_PREFIX)
        .tempfile_in(&dir)
        .map_err(|error| refused(&dir, &error))?;
    let failed = |error: io::Error| {
        tracing::debug!(error = %error, "the new binary could not be put in place");
        UpdateError::Replace { dir: dir.clone() }
    };
    temp.write_all(bytes).map_err(failed)?;
    temp.as_file().sync_all().map_err(failed)?;
    make_executable(temp.path()).map_err(failed)?;
    put_in_place(temp, binary).map_err(failed)
}

/// The name the installed binary is renamed to while the new one takes its place:
/// `dibs.exe` → `dibs.old.exe`, `dibs` → `dibs.old`.
pub fn aside_path(binary: &Path) -> PathBuf {
    let stem = binary
        .file_stem()
        .map_or_else(|| "dibs".into(), |stem| stem.to_os_string());
    let mut name = stem;
    name.push(".old");
    if let Some(extension) = binary.extension() {
        name.push(".");
        name.push(extension);
    }
    binary.with_file_name(name)
}

/// Deletes the binary an update renamed aside, when one is left; anything that keeps it from
/// being deleted (it still runs, it is gone) is left for a later start.
pub fn remove_previous(binary: &Path) {
    let aside = aside_path(binary);
    if let Err(error) = fs::remove_file(&aside)
        && error.kind() != io::ErrorKind::NotFound
    {
        tracing::debug!(error = %error, "the binary an update replaced could not be deleted yet");
    }
}

/// Renames `binary` to `aside`, then `new` to `binary`; when the second step fails, `aside` is
/// renamed back. A file already at `aside` is deleted first.
///
/// # Errors
/// The file system's failure; `binary` is then where it was.
pub fn swap_by_rename(binary: &Path, new: &Path, aside: &Path) -> io::Result<()> {
    match fs::remove_file(aside) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    fs::rename(binary, aside)?;
    if let Err(error) = fs::rename(new, binary) {
        if let Err(undo) = fs::rename(aside, binary) {
            tracing::warn!(error = %undo, "the installed binary could not be renamed back");
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(not(windows))]
fn put_in_place(temp: tempfile::NamedTempFile, binary: &Path) -> io::Result<()> {
    temp.persist(binary).map(drop).map_err(|error| error.error)
}

#[cfg(windows)]
fn put_in_place(temp: tempfile::NamedTempFile, binary: &Path) -> io::Result<()> {
    let new = temp.into_temp_path();
    if !binary.exists() {
        return new.persist(binary).map_err(|error| error.error);
    }
    // Keeping the file has to come before it moves: on Windows it touches the file at its path.
    let kept = new.keep().map_err(|error| error.error)?;
    swap_by_rename(binary, &kept, &aside_path(binary)).inspect_err(|_| {
        if let Err(error) = fs::remove_file(&kept) {
            tracing::debug!(error = %error, "the unused new binary could not be deleted");
        }
    })
}

#[cfg(unix)]
fn make_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn refused(dir: &Path, error: &io::Error) -> UpdateError {
    tracing::debug!(error = %error, "the install directory cannot be written");
    if error.kind() == io::ErrorKind::PermissionDenied {
        UpdateError::Unwritable {
            dir: dir.to_path_buf(),
        }
    } else {
        UpdateError::Replace {
            dir: dir.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_installed_binary_is_replaced_and_no_temporary_file_is_left() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("dibs");
        fs::write(&binary, b"old").unwrap();
        replace(&binary, b"new").unwrap();
        assert_eq!(fs::read(&binary).unwrap(), b"new");
        remove_previous(&binary);
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&binary).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
        }
    }

    #[test]
    fn the_aside_name_keeps_the_extension() {
        assert_eq!(
            aside_path(Path::new("/opt/dibs/dibs.exe")),
            PathBuf::from("/opt/dibs/dibs.old.exe")
        );
        assert_eq!(
            aside_path(Path::new("/opt/dibs/dibs")),
            PathBuf::from("/opt/dibs/dibs.old")
        );
    }

    #[test]
    fn the_rename_swap_moves_the_old_binary_aside_and_the_next_start_deletes_it() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("dibs.exe");
        let new = dir.path().join(".dibs-update-1");
        let aside = aside_path(&binary);
        fs::write(&binary, b"old").unwrap();
        fs::write(&aside, b"older still").unwrap();
        fs::write(&new, b"new").unwrap();
        swap_by_rename(&binary, &new, &aside).unwrap();
        assert_eq!(fs::read(&binary).unwrap(), b"new");
        assert_eq!(fs::read(&aside).unwrap(), b"old");
        assert!(!new.exists());
        remove_previous(&binary);
        assert!(!aside.exists());
        remove_previous(&binary);
    }

    #[test]
    fn a_swap_whose_second_rename_fails_puts_the_old_binary_back() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("dibs.exe");
        fs::write(&binary, b"old").unwrap();
        let missing = dir.path().join("no-such-file");
        assert!(swap_by_rename(&binary, &missing, &aside_path(&binary)).is_err());
        assert_eq!(fs::read(&binary).unwrap(), b"old");
        assert!(!aside_path(&binary).exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_this_user_cannot_write_refuses_with_the_install_hint() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let binary = bin.join("dibs");
        fs::write(&binary, b"old").unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o555)).unwrap();
        if tempfile::tempfile_in(&bin).is_ok() {
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }
        let refusal = check_writable(&bin).unwrap_err();
        assert!(
            matches!(refusal, UpdateError::Unwritable { .. }),
            "{refusal:?}"
        );
        assert!(refusal.to_string().contains("install script"), "{refusal}");
        assert!(matches!(
            replace(&binary, b"new"),
            Err(UpdateError::Unwritable { .. })
        ));
        assert_eq!(fs::read(&binary).unwrap(), b"old");
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    }
}
