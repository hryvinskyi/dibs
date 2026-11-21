//! The secret this machine keys its checkouts' tokens with: random bytes made once and kept in the
//! state directory as `checkout-key`, beside the machine's id, in a file only its user may read. It
//! is never sent anywhere, so a token says nothing of where its checkout is to anyone without the
//! machine.

use std::fs::{self, OpenOptions};
use std::path::Path;

use rand::RngExt as _;

use dibs_client::checkout_token::CheckoutKey;
use dibs_client::private_file::{self, Principal};

/// The file the key is kept in, under the state directory.
const FILE: &str = "checkout-key";

/// What reading the kept key came to.
enum Kept {
    /// A key was read.
    Key(CheckoutKey),
    /// There is none, or what the file holds is no key: one is made.
    Missing,
    /// The file cannot be read, or other users could read it: no key is used.
    Unusable,
}

/// This machine's checkout key, made and kept, private to `principal`, the first time it is asked
/// for; `None` when it can be neither read nor kept, or when the file holding it could be read by
/// other users, in which case no checkout is named.
pub fn checkout_key(state_dir: &Path, principal: &Principal) -> Option<CheckoutKey> {
    let path = state_dir.join(FILE);
    match read(&path, principal) {
        Kept::Key(key) => return Some(key),
        Kept::Unusable => return None,
        Kept::Missing => {}
    }
    fs::create_dir_all(state_dir).ok()?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(state_dir.join(format!("{FILE}.lock")))
        .ok()?;
    lock.lock().ok()?;
    match read(&path, principal) {
        Kept::Key(key) => return Some(key),
        Kept::Unusable => return None,
        Kept::Missing => {}
    }
    let key = CheckoutKey::from_bytes(rand::rng().random());
    match private_file::write(&path, &format!("{}\n", key.to_hex()), principal) {
        Ok(()) => Some(key),
        Err(error) => {
            tracing::debug!(error = %error, "this machine's checkout key could not be kept");
            None
        }
    }
}

fn read(path: &Path, principal: &Principal) -> Kept {
    match private_file::read(path, principal) {
        Ok(Some(text)) => CheckoutKey::parse(&text).map_or(Kept::Missing, Kept::Key),
        Ok(None) => Kept::Missing,
        Err(error) => {
            tracing::debug!(error = %error, "this machine's checkout key could not be read");
            Kept::Unusable
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal() -> Principal {
        Principal::from_env(&|var| std::env::var(var).ok())
    }

    #[test]
    fn the_key_is_made_once_kept_and_differs_between_machines() {
        let dir = tempfile::tempdir().unwrap();
        let first = checkout_key(dir.path(), &principal()).unwrap();
        assert_eq!(checkout_key(dir.path(), &principal()), Some(first.clone()));
        let other = tempfile::tempdir().unwrap();
        assert_ne!(checkout_key(other.path(), &principal()), Some(first));
    }

    #[cfg(unix)]
    #[test]
    fn only_its_user_may_read_the_key_and_one_others_could_read_is_not_used() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let key = checkout_key(dir.path(), &principal()).unwrap();
        let path = dir.path().join(FILE);
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(checkout_key(dir.path(), &principal()), None);
        assert_eq!(
            CheckoutKey::parse(&fs::read_to_string(&path).unwrap()),
            Some(key),
            "the key is left as it was"
        );
    }

    #[test]
    fn a_file_that_holds_no_key_is_replaced_by_a_new_one() {
        let dir = tempfile::tempdir().unwrap();
        private_file::write(&dir.path().join(FILE), "not a key\n", &principal()).unwrap();
        let made = checkout_key(dir.path(), &principal()).unwrap();
        assert_eq!(checkout_key(dir.path(), &principal()), Some(made));
    }
}
