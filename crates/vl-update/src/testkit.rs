//! Test helpers: a throwaway minisign key pair made inside the test, and synthetic manifests.

use std::io::Cursor;

use minisign::KeyPair;

use crate::UpdateKey;

/// A key pair generated for one test; it never leaves the process.
pub(crate) struct TestSigner(KeyPair);

impl TestSigner {
    pub(crate) fn new() -> Self {
        Self(KeyPair::generate_unencrypted_keypair().unwrap())
    }

    /// The public key as the launcher reads it.
    pub(crate) fn key(&self) -> UpdateKey {
        UpdateKey::from_text(&self.0.pk.to_base64()).unwrap()
    }

    /// The whole `minisign.pub` text.
    pub(crate) fn public_file(&self) -> String {
        self.0.pk.to_box().unwrap().into_string()
    }

    /// A `.minisig` text for `data` (prehashed, as current minisign signs).
    pub(crate) fn sign(&self, data: &[u8]) -> String {
        minisign::sign(
            Some(&self.0.pk),
            &self.0.sk,
            Cursor::new(data),
            Some("timestamp:0\tfile:latest.json"),
            Some("signature from a test key"),
        )
        .unwrap()
        .into_string()
    }

    /// The same signature marked as the legacy (not prehashed) algorithm: the algorithm bytes `ED`
    /// become `Ed`, which is `RU…` to `RW…` in base64.
    pub(crate) fn sign_legacy(&self, data: &[u8]) -> String {
        let sig = self.sign(data);
        let mut lines: Vec<String> = sig.lines().map(str::to_owned).collect();
        assert!(lines[1].starts_with("RU"), "{}", lines[1]);
        lines[1].replace_range(1..2, "W");
        lines.join("\n") + "\n"
    }
}

/// A valid manifest for version `version` with one artifact per platform and kind used in tests.
pub(crate) fn manifest_json(version: &str) -> String {
    let artifact = |os: &str, arch: &str, kind: &str, ext: &str| {
        format!(
            r#"{{"os":"{os}","arch":"{arch}","kind":"{kind}","url":"https://downloads.example.test/{version}/vibelauncher-{os}-{arch}.{ext}","sha256":"{}","size":1000}}"#,
            "ab".repeat(32)
        )
    };
    let artifacts = [
        artifact("windows", "x86_64", "msi", "msi"),
        artifact("windows", "x86_64", "portable", "zip"),
        artifact("mac_os", "aarch64", "dmg", "dmg"),
        artifact("linux", "x86_64", "app_image", "AppImage"),
        artifact("linux", "x86_64", "deb", "deb"),
        artifact("linux", "x86_64", "portable", "zip"),
    ]
    .join(",");
    format!(
        r#"{{"schema":1,"channel":"stable","version":"{version}","date":"2026-10-01","minimum_version":"0.1.0","page":"https://example.test/releases/{version}","artifacts":[{artifacts}]}}"#
    )
}
