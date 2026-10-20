//! The release key a build trusts: a minisign public key, in the two-line format of a
//! `minisign.pub` file. Text that is no such key — the placeholder a build carries before a key
//! was made — trusts nothing, and every update refuses.

use minisign_verify::{PublicKey, Signature};

use crate::update::UpdateError;

/// The key releases are verified against, when the build carries one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedKey(Option<PublicKey>);

/// A key that can verify.
#[derive(Debug, Clone, Copy)]
pub struct Verifier<'a>(&'a PublicKey);

impl TrustedKey {
    /// The key a `minisign.pub` text holds; no key at all when the text holds none.
    pub fn parse(text: &str) -> Self {
        Self(PublicKey::decode(text.trim()).ok())
    }

    /// Whether the build carries a key.
    pub fn is_set(&self) -> bool {
        self.0.is_some()
    }

    /// The key, ready to verify.
    ///
    /// # Errors
    /// [`UpdateError::NoKey`] when the build carries none.
    pub fn verifier(&self) -> Result<Verifier<'_>, UpdateError> {
        self.0.as_ref().map(Verifier).ok_or(UpdateError::NoKey)
    }
}

impl Verifier<'_> {
    /// Checks that `signature`, the text of a `.minisig` file, signs `data` with this key, in
    /// minisign's pre-hashed mode.
    ///
    /// # Errors
    /// [`UpdateError::BadSignature`] when it does not, or when it is no signature.
    pub fn verify(&self, data: &[u8], signature: &[u8]) -> Result<(), UpdateError> {
        let text = std::str::from_utf8(signature).map_err(|_| UpdateError::BadSignature)?;
        let signature = Signature::decode(text).map_err(|_| UpdateError::BadSignature)?;
        self.0
            .verify(data, &signature, false)
            .map_err(|_| UpdateError::BadSignature)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn pair() -> (minisign::KeyPair, String) {
        let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let text = pair.pk.to_box().unwrap().to_string();
        (pair, text)
    }

    fn sign(pair: &minisign::KeyPair, data: &[u8]) -> Vec<u8> {
        minisign::sign(Some(&pair.pk), &pair.sk, Cursor::new(data), None, None)
            .unwrap()
            .to_string()
            .into_bytes()
    }

    #[test]
    fn text_that_holds_no_key_trusts_nothing() {
        for text in [
            "",
            "untrusted comment: no release signing key has been made yet\nplaceholder\n",
            "RWQ not base64 at all",
        ] {
            let key = TrustedKey::parse(text);
            assert!(!key.is_set(), "{text:?}");
            assert_eq!(key.verifier().unwrap_err(), UpdateError::NoKey);
        }
    }

    #[test]
    fn a_signature_by_the_key_verifies_and_any_other_does_not() {
        let (ours, text) = pair();
        let key = TrustedKey::parse(&text);
        let verifier = key.verifier().unwrap();
        let data = b"abc  dibs-1.0.0-x86_64-unknown-linux-gnu.tar.gz\n";
        verifier.verify(data, &sign(&ours, data)).unwrap();
        let (other, _) = pair();
        for (signature, what) in [
            (sign(&other, data), "another key"),
            (sign(&ours, b"other data"), "other data"),
            (b"not a signature".to_vec(), "garbage"),
            (vec![0xff, 0xfe], "not text"),
        ] {
            assert_eq!(
                verifier.verify(data, &signature),
                Err(UpdateError::BadSignature),
                "{what}"
            );
        }
    }
}
