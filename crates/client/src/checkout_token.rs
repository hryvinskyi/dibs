//! The token a checkout is named by when the server is told which checkout a session works in: an
//! HMAC-SHA-256 of the checkout's root directory, keyed with a secret that never leaves the
//! member's machine, so the server can tell two of a member's checkouts apart without learning
//! where either is, nor guess it from a path and anything else the client sends.

use std::fmt;
use std::path::Path;

use ring::hmac;

use dibs_domain::session::{CheckoutToken, CheckoutTokenError};

/// The secret a machine keys its checkouts' tokens with: random bytes made on the machine and
/// kept there, sent nowhere. Its bytes are never printed.
#[derive(Clone, PartialEq, Eq)]
pub struct CheckoutKey([u8; CheckoutKey::BYTES]);

impl CheckoutKey {
    /// How many bytes a key holds.
    pub const BYTES: usize = 32;

    /// The key made of `bytes`, which the caller drew at random.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; Self::BYTES]) -> Self {
        Self(bytes)
    }

    /// The key `text` spells as [`CheckoutKey::to_hex`] writes it, surrounding whitespace aside;
    /// `None` for any other text.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.len() != Self::BYTES * 2 {
            return None;
        }
        let mut bytes = [0; Self::BYTES];
        let (pairs, _) = text.as_bytes().as_chunks::<2>();
        for (byte, [high, low]) in bytes.iter_mut().zip(pairs) {
            *byte = (hex_digit(*high)? << 4) | hex_digit(*low)?;
        }
        Some(Self(bytes))
    }

    /// The key as lowercase hexadecimal characters, two per byte.
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }
}

impl fmt::Debug for CheckoutKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CheckoutKey(..)")
    }
}

/// The value of one lowercase hexadecimal digit.
const fn hex_digit(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        _ => None,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The token of the checkout whose root is `root` on the machine that keeps `key`: the first 32
/// lowercase hexadecimal characters of the HMAC-SHA-256 of the root's path under the key. The same
/// root and key always give the same token, and another root or another key another; `root` is
/// taken as given, so a caller passes it in the one spelling the file system resolves it to.
///
/// # Errors
/// None in practice: the digest is always written as a valid token.
pub fn checkout_token(key: &CheckoutKey, root: &Path) -> Result<CheckoutToken, CheckoutTokenError> {
    let key = hmac::Key::new(hmac::HMAC_SHA256, &key.0);
    let tag = hmac::sign(&key, root.to_string_lossy().as_bytes());
    CheckoutToken::parse(&hex(&tag.as_ref()[..CheckoutToken::CHARS / 2]))
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::*;

    fn key(fill: u8) -> CheckoutKey {
        CheckoutKey::from_bytes([fill; CheckoutKey::BYTES])
    }

    #[test]
    fn a_token_is_32_lowercase_hex_characters_stable_per_key_and_root() {
        let widgets = Path::new("/home/alice/widgets");
        let token = checkout_token(&key(1), widgets).unwrap();
        assert_eq!(token.as_str().len(), 32);
        assert!(
            token
                .as_str()
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        );
        assert_eq!(checkout_token(&key(1), widgets).unwrap(), token, "stable");
        let worktree = Path::new("/home/alice/widgets-worktrees/worktree1");
        assert_ne!(
            checkout_token(&key(1), worktree).unwrap(),
            token,
            "another root"
        );
        assert_ne!(
            checkout_token(&key(2), widgets).unwrap(),
            token,
            "another key"
        );
    }

    #[test]
    fn the_token_is_the_hmac_sha256_of_the_root_under_the_key() {
        let root = Path::new("/srv/acme/api");
        // RFC 2104 by hand over the standard SHA-256, so the construction is pinned and not only
        // the library call that makes it.
        let pad =
            |fill: u8| -> Vec<u8> { [7u8; 32].iter().chain(&[0; 32]).map(|b| b ^ fill).collect() };
        let inner = Sha256::new()
            .chain_update(pad(0x36))
            .chain_update(b"/srv/acme/api")
            .finalize();
        let outer = Sha256::new()
            .chain_update(pad(0x5c))
            .chain_update(inner)
            .finalize();
        assert_eq!(
            checkout_token(&key(7), root).unwrap().as_str(),
            hex(&outer[..16])
        );
    }

    #[test]
    fn a_key_reads_back_as_written_and_nothing_else_reads_as_one() {
        let made = CheckoutKey::from_bytes(std::array::from_fn(|at| u8::try_from(at).unwrap() * 7));
        let written = made.to_hex();
        assert_eq!(written.len(), 64);
        assert_eq!(
            CheckoutKey::parse(&format!("{written}\n")),
            Some(made.clone())
        );
        assert_eq!(CheckoutKey::parse(&written[..62]), None, "too short");
        assert_eq!(
            CheckoutKey::parse(&written.to_uppercase()),
            None,
            "not lowercase"
        );
        assert_eq!(CheckoutKey::parse(&"g".repeat(64)), None, "not hexadecimal");
        assert_eq!(format!("{made:?}"), "CheckoutKey(..)", "never printed");
    }
}
