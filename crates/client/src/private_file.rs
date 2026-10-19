//! Files only their user may read: written so from their first byte, and refused when anyone else
//! could read them.
//!
//! On Unix that is mode `0600` in a `0700` directory; group or world bits refuse the file. On
//! Windows it is a protected access list granting the current user alone, set on the directory
//! (so a file created in it inherits it) and on the file before its content is written; an entry
//! that lets another principal read refuses the file.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use thiserror::Error;

/// The user files are made private to: on Windows the account the process runs as, named by
/// `USERDOMAIN` and `USERNAME`; on Unix the file's mode says it all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    account: Option<String>,
}

impl Principal {
    /// The current user, as the environment names them.
    pub fn from_env(lookup: crate::locations::Lookup<'_>) -> Self {
        let user = lookup("USERNAME").filter(|name| !name.trim().is_empty());
        let account = match (lookup("USERDOMAIN"), user) {
            (Some(domain), Some(user)) if !domain.trim().is_empty() => {
                Some(format!("{domain}\\{user}"))
            }
            (_, user) => user,
        };
        Self { account }
    }

    /// The account, `DOMAIN\user` or `user`, when the environment names one.
    pub fn account(&self) -> Option<&str> {
        self.account.as_deref()
    }
}

/// Why a private file could not be read or written.
#[derive(Debug, Error)]
pub enum PrivateFileError {
    /// Another user could read the file.
    #[error("{path} can be read by other users; {fix}")]
    Exposed {
        /// The file.
        path: PathBuf,
        /// How to make it private again.
        fix: String,
    },
    /// The file or its directory could not be read or written.
    #[error("{path} could not be {action}")]
    Io {
        /// The file.
        path: PathBuf,
        /// `read` or `written`.
        action: &'static str,
        /// Why.
        #[source]
        source: io::Error,
    },
}

/// The file's text when only its user may read it; `None` when it does not exist.
///
/// # Errors
/// [`PrivateFileError::Exposed`] when anyone else may read it; [`PrivateFileError::Io`].
pub fn read(path: &Path, principal: &Principal) -> Result<Option<String>, PrivateFileError> {
    if !path.exists() {
        return Ok(None);
    }
    platform::check(path, principal)?;
    fs::read_to_string(path)
        .map(Some)
        .map_err(|source| PrivateFileError::Io {
            path: path.to_path_buf(),
            action: "read",
            source,
        })
}

/// Replaces the file's content in one step, the file private to its user from the start: the
/// text goes to a private temporary file beside it, which is then renamed over it.
///
/// # Errors
/// [`PrivateFileError::Io`].
pub fn write(path: &Path, text: &str, principal: &Principal) -> Result<(), PrivateFileError> {
    let io_error = |source: io::Error| PrivateFileError::Io {
        path: path.to_path_buf(),
        action: "written",
        source,
    };
    let dir = path
        .parent()
        .ok_or_else(|| io_error(io::Error::from(io::ErrorKind::InvalidInput)))?;
    platform::create_dir(dir, principal).map_err(io_error)?;
    let mut temp = tempfile::Builder::new()
        .prefix(".dibs-")
        .tempfile_in(dir)
        .map_err(io_error)?;
    platform::restrict(temp.path(), principal).map_err(io_error)?;
    temp.write_all(text.as_bytes()).map_err(io_error)?;
    temp.as_file().sync_all().map_err(io_error)?;
    temp.persist(path).map_err(|error| io_error(error.error))?;
    Ok(())
}

#[cfg(unix)]
mod platform {
    use std::fs::{self, DirBuilder, Permissions};
    use std::io;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    use std::path::Path;

    use super::{Principal, PrivateFileError};

    pub(super) fn check(path: &Path, _principal: &Principal) -> Result<(), PrivateFileError> {
        let mode = fs::metadata(path)
            .map_err(|source| PrivateFileError::Io {
                path: path.to_path_buf(),
                action: "read",
                source,
            })?
            .permissions()
            .mode();
        if mode & 0o077 == 0 {
            return Ok(());
        }
        Err(PrivateFileError::Exposed {
            path: path.to_path_buf(),
            fix: format!("run chmod 600 {}", path.display()),
        })
    }

    pub(super) fn create_dir(dir: &Path, _principal: &Principal) -> io::Result<()> {
        if dir.is_dir() {
            return Ok(());
        }
        DirBuilder::new().recursive(true).mode(0o700).create(dir)
    }

    pub(super) fn restrict(path: &Path, _principal: &Principal) -> io::Result<()> {
        fs::set_permissions(path, Permissions::from_mode(0o600))
    }
}

#[cfg(windows)]
mod platform {
    use std::ffi::OsString;
    use std::fs;
    use std::io;
    use std::path::Path;

    use windows_permissions::constants::{
        AccessRights, AceType, SeObjectType, SecurityInformation,
    };
    use windows_permissions::wrappers::{
        GetNamedSecurityInfo, LookupAccountName, SetNamedSecurityInfo,
    };
    use windows_permissions::{LocalBox, SecurityDescriptor, Sid};

    use super::{Principal, PrivateFileError};

    /// The rights that let a principal read a file's content.
    fn reading() -> AccessRights {
        AccessRights::Bit0 | AccessRights::GenericRead | AccessRights::GenericAll
    }

    fn user_sid(principal: &Principal) -> io::Result<Box<Sid>> {
        let account = principal.account().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "the current user is not known")
        })?;
        let (sid, _, _) = LookupAccountName(None::<OsString>, account)?;
        Ok(sid)
    }

    pub(super) fn check(path: &Path, principal: &Principal) -> Result<(), PrivateFileError> {
        let read_error = |source: io::Error| PrivateFileError::Io {
            path: path.to_path_buf(),
            action: "read",
            source,
        };
        let exposed = || PrivateFileError::Exposed {
            path: path.to_path_buf(),
            fix: "remove it and sign in again with dibs login".to_owned(),
        };
        let user = user_sid(principal).map_err(read_error)?;
        let descriptor = GetNamedSecurityInfo(
            path.as_os_str(),
            SeObjectType::SE_FILE_OBJECT,
            SecurityInformation::Dacl,
        )
        .map_err(read_error)?;
        let Some(dacl) = descriptor.dacl() else {
            return Err(exposed());
        };
        for index in 0..dacl.len() {
            let Some(ace) = dacl.get_ace(index) else {
                continue;
            };
            let allows = matches!(
                ace.ace_type(),
                AceType::ACCESS_ALLOWED_ACE_TYPE
                    | AceType::ACCESS_ALLOWED_CALLBACK_ACE_TYPE
                    | AceType::ACCESS_ALLOWED_OBJECT_ACE_TYPE
                    | AceType::ACCESS_ALLOWED_CALLBACK_OBJECT_ACE_TYPE
            );
            if !allows || !ace.mask().intersects(reading()) {
                continue;
            }
            if ace.sid() != Some(&*user) {
                return Err(exposed());
            }
        }
        Ok(())
    }

    fn user_only(principal: &Principal, inherit: bool) -> io::Result<LocalBox<SecurityDescriptor>> {
        let user = user_sid(principal)?;
        let flags = if inherit { "OICI" } else { "" };
        format!("D:P(A;{flags};FA;;;{user})").parse::<LocalBox<SecurityDescriptor>>()
    }

    fn apply(path: &Path, descriptor: &SecurityDescriptor) -> io::Result<()> {
        let dacl = descriptor.dacl().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "no access list was built")
        })?;
        SetNamedSecurityInfo(
            path.as_os_str(),
            SeObjectType::SE_FILE_OBJECT,
            SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
            None,
            None,
            Some(dacl),
            None,
        )
    }

    pub(super) fn create_dir(dir: &Path, principal: &Principal) -> io::Result<()> {
        if !dir.is_dir() {
            fs::create_dir_all(dir)?;
        }
        let descriptor = user_only(principal, true)?;
        apply(dir, &descriptor)
    }

    pub(super) fn restrict(path: &Path, principal: &Principal) -> io::Result<()> {
        let descriptor = user_only(principal, false)?;
        apply(path, &descriptor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal() -> Principal {
        Principal::from_env(&|var| std::env::var(var).ok())
    }

    #[test]
    fn a_written_file_reads_back_and_a_missing_one_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config").join("credentials.toml");
        assert!(read(&path, &principal()).unwrap().is_none());
        write(&path, "one", &principal()).unwrap();
        write(&path, "two", &principal()).unwrap();
        assert_eq!(read(&path, &principal()).unwrap().as_deref(), Some("two"));
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_0600_in_a_0700_directory() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config").join("credentials.toml");
        write(&path, "secret", &principal()).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn a_file_others_may_read_is_refused_with_the_fix() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.toml");
        write(&path, "secret", &principal()).unwrap();
        for mode in [0o644, 0o640, 0o604, 0o660] {
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            let error = read(&path, &principal()).unwrap_err();
            assert!(
                matches!(error, PrivateFileError::Exposed { .. }),
                "{mode:o}"
            );
            assert!(error.to_string().contains("chmod 600"), "{error}");
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
        assert_eq!(
            read(&path, &principal()).unwrap().as_deref(),
            Some("secret")
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_file_with_the_inherited_access_list_is_refused_and_a_written_one_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let loose = dir.path().join("loose.toml");
        fs::write(&loose, "secret").unwrap();
        let error = read(&loose, &principal()).unwrap_err();
        assert!(matches!(error, PrivateFileError::Exposed { .. }), "{error}");

        let path = dir.path().join("config").join("credentials.toml");
        write(&path, "secret", &principal()).unwrap();
        assert_eq!(
            read(&path, &principal()).unwrap().as_deref(),
            Some("secret")
        );
    }
}
