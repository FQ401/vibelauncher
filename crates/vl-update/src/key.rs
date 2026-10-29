//! The release public key and minisign signature checks.

use minisign_verify::{PublicKey, Signature};

use crate::UpdateError;

/// The minisign public key (the base64 line of a `minisign.pub` file) that release manifests are
/// signed with, embedded in the binary.
///
/// `None` in a build without a release key: every update check then fails closed with
/// [`UpdateError::NoReleaseKey`]. The key is set at compile time from the
/// `VL_UPDATE_PUBLIC_KEY` environment variable; an empty value counts as none.
pub const RELEASE_PUBLIC_KEY: Option<&str> = match option_env!("VL_UPDATE_PUBLIC_KEY") {
    Some(key) if !key.is_empty() => Some(key),
    _ => None,
};

/// The largest signature file accepted (a minisign signature is about 300 bytes).
pub const MAX_SIGNATURE_BYTES: usize = 2 * 1024;

/// A public key that manifests are verified with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateKey(PublicKey);

impl UpdateKey {
    /// The embedded release key ([`RELEASE_PUBLIC_KEY`]).
    pub fn release() -> Result<Self, UpdateError> {
        Self::from_text(RELEASE_PUBLIC_KEY.ok_or(UpdateError::NoReleaseKey)?)
    }

    /// A key from the base64 line of a `minisign.pub` file, or from the whole file (comment line
    /// and key line).
    pub fn from_text(text: &str) -> Result<Self, UpdateError> {
        let text = text.trim();
        let key = if text.contains('\n') {
            PublicKey::decode(text)
        } else {
            PublicKey::from_base64(text)
        };
        key.map(Self).map_err(|_| UpdateError::BadKey)
    }

    /// Checks that `signature` (the text of a `.minisig` file) is a valid signature of `data` by
    /// this key. Only the prehashed (BLAKE2b) algorithm of current minisign is accepted; legacy
    /// signatures are refused.
    pub fn verify(&self, data: &[u8], signature: &[u8]) -> Result<(), UpdateError> {
        if signature.len() > MAX_SIGNATURE_BYTES {
            return Err(UpdateError::TooLarge {
                what: "signature",
                limit: MAX_SIGNATURE_BYTES,
            });
        }
        let text = std::str::from_utf8(signature)
            .map_err(|_| UpdateError::BadSignature("not text".into()))?;
        let signature =
            Signature::decode(text).map_err(|e| UpdateError::BadSignature(e.to_string()))?;
        self.0
            .verify(data, &signature, false)
            .map_err(|e| UpdateError::BadSignature(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::TestSigner;

    #[test]
    fn this_build_has_no_release_key_and_fails_closed() {
        // The release key does not exist yet; a build that sets one must update this test.
        if RELEASE_PUBLIC_KEY.is_none() {
            assert_eq!(UpdateKey::release(), Err(UpdateError::NoReleaseKey));
        }
    }

    #[test]
    fn accepts_a_valid_signature() {
        let signer = TestSigner::new();
        let data = b"some bytes";
        let sig = signer.sign(data);
        signer.key().verify(data, sig.as_bytes()).unwrap();
        // The full `minisign.pub` text works as well as its key line.
        let full = UpdateKey::from_text(&signer.public_file()).unwrap();
        full.verify(data, sig.as_bytes()).unwrap();
    }

    #[test]
    fn rejects_changed_data_another_key_and_damaged_signatures() {
        let signer = TestSigner::new();
        let data = b"some bytes";
        let sig = signer.sign(data);
        let key = signer.key();
        assert!(matches!(
            key.verify(b"some bytez", sig.as_bytes()),
            Err(UpdateError::BadSignature(_))
        ));
        let other = TestSigner::new();
        assert!(matches!(
            other.key().verify(data, sig.as_bytes()),
            Err(UpdateError::BadSignature(_))
        ));
        // A changed trusted comment breaks the global signature.
        let forged = sig.replace("trusted comment: ", "trusted comment: x");
        assert!(matches!(
            key.verify(data, forged.as_bytes()),
            Err(UpdateError::BadSignature(_))
        ));
        for bad in ["", "garbage", "untrusted comment: x\n!!!\n"] {
            assert!(matches!(
                key.verify(data, bad.as_bytes()),
                Err(UpdateError::BadSignature(_))
            ));
        }
        assert!(matches!(
            key.verify(data, &[0xff, 0xfe]),
            Err(UpdateError::BadSignature(_))
        ));
        let huge = "a".repeat(MAX_SIGNATURE_BYTES + 1);
        assert!(matches!(
            key.verify(data, huge.as_bytes()),
            Err(UpdateError::TooLarge { .. })
        ));
    }

    #[test]
    fn rejects_legacy_signatures() {
        let signer = TestSigner::new();
        let data = b"some bytes";
        let legacy = signer.sign_legacy(data);
        assert!(matches!(
            signer.key().verify(data, legacy.as_bytes()),
            Err(UpdateError::BadSignature(_))
        ));
    }

    #[test]
    fn rejects_malformed_keys() {
        for bad in ["", "not base64", "RWQ=", "untrusted comment\nnot a key"] {
            assert_eq!(UpdateKey::from_text(bad), Err(UpdateError::BadKey), "{bad}");
        }
    }
}
