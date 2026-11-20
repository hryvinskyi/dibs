//! The token a checkout is named by when the server is told which checkout a session works in:
//! a digest of the machine's id and the checkout's root directory, so the server can tell two of
//! a member's checkouts apart without learning where either is.

use std::path::Path;

use sha2::{Digest, Sha256};

use dibs_domain::report::MachineId;
use dibs_domain::session::{CheckoutToken, CheckoutTokenError};

/// The token of the checkout whose root is `root` on the machine `machine`: the first 32 lowercase
/// hexadecimal characters of the SHA-256 of the machine's id, a NUL byte and the root's path. The
/// same root on the same machine always gives the same token, and another root or another machine
/// another; `root` is taken as given, so a caller passes it in the one spelling the file system
/// resolves it to.
///
/// # Errors
/// None in practice: the digest is always written as a valid token.
pub fn checkout_token(
    machine: &MachineId,
    root: &Path,
) -> Result<CheckoutToken, CheckoutTokenError> {
    let mut digest = Sha256::new();
    digest.update(machine.as_str().as_bytes());
    digest.update([0]);
    digest.update(root.to_string_lossy().as_bytes());
    let hex: String = digest.finalize()[..CheckoutToken::CHARS / 2]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    CheckoutToken::parse(&hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_is_32_lowercase_hex_characters_stable_per_machine_and_root() {
        let laptop = MachineId::from_bits(1);
        let desktop = MachineId::from_bits(2);
        let widgets = Path::new("/home/alice/widgets");
        let token = checkout_token(&laptop, widgets).unwrap();
        assert_eq!(token.as_str().len(), 32);
        assert!(
            token
                .as_str()
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        );
        assert_eq!(checkout_token(&laptop, widgets).unwrap(), token, "stable");
        let worktree = Path::new("/home/alice/widgets-worktrees/worktree1");
        assert_ne!(
            checkout_token(&laptop, worktree).unwrap(),
            token,
            "another root"
        );
        assert_ne!(
            checkout_token(&desktop, widgets).unwrap(),
            token,
            "another machine"
        );
    }

    #[test]
    fn the_token_is_the_digest_of_the_machine_a_nul_and_the_root() {
        let machine = MachineId::from_bits(0xabc);
        let root = Path::new("/srv/acme/api");
        let mut text = machine.as_str().as_bytes().to_vec();
        text.push(0);
        text.extend_from_slice(b"/srv/acme/api");
        let expected: String = Sha256::digest(&text)[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(checkout_token(&machine, root).unwrap().as_str(), expected);
    }
}
