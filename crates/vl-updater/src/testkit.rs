//! Test helpers: temporary folders, a throwaway signing key, a fake HTTP server and archives.

use std::collections::HashMap;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use minisign::KeyPair;
use vl_net::{HttpRequest, HttpResponse, HttpTransport, NetError};
use vl_update::UpdateKey;

/// A temporary folder removed on drop.
pub(crate) struct Dir(PathBuf);

impl Dir {
    pub(crate) fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vl-updater-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A key pair generated for one test; it never leaves the process.
pub(crate) struct TestSigner(KeyPair);

impl TestSigner {
    pub(crate) fn new() -> Self {
        Self(KeyPair::generate_unencrypted_keypair().unwrap())
    }

    pub(crate) fn key(&self) -> UpdateKey {
        UpdateKey::from_text(&self.0.pk.to_base64()).unwrap()
    }

    pub(crate) fn sign(&self, data: &[u8]) -> Vec<u8> {
        minisign::sign(
            Some(&self.0.pk),
            &self.0.sk,
            Cursor::new(data),
            Some("timestamp:0\tfile:latest.json"),
            None,
        )
        .unwrap()
        .into_string()
        .into_bytes()
    }
}

/// Serves bytes by URL; anything else is `404`.
#[derive(Default)]
pub(crate) struct Files {
    files: Mutex<HashMap<String, Vec<u8>>>,
    pub(crate) fetched: Mutex<Vec<String>>,
}

impl Files {
    pub(crate) fn put(&self, url: &str, bytes: impl Into<Vec<u8>>) {
        self.files
            .lock()
            .unwrap()
            .insert(url.to_owned(), bytes.into());
    }
}

impl HttpTransport for Files {
    fn get(&self, request: &HttpRequest) -> Result<HttpResponse, NetError> {
        self.fetched.lock().unwrap().push(request.url.clone());
        let body = self.files.lock().unwrap().get(&request.url).cloned();
        Ok(match body {
            Some(b) => HttpResponse {
                status: 200,
                headers: vec![("content-length".into(), b.len().to_string())],
                body: Box::new(Cursor::new(b)),
            },
            None => HttpResponse {
                status: 404,
                headers: Vec::new(),
                body: Box::new(Cursor::new(Vec::new())),
            },
        })
    }
}

/// A zip archive with `(name, content)` entries; a name ending in `/` is a folder.
pub(crate) fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    for (name, content) in entries {
        if let Some(dir) = name.strip_suffix('/') {
            w.add_directory(dir, options).unwrap();
        } else {
            w.start_file(*name, options).unwrap();
            w.write_all(content).unwrap();
        }
    }
    w.finish().unwrap().into_inner()
}

/// The lowercase hex SHA-256 of `data`.
pub(crate) fn sha256(data: &[u8]) -> String {
    vl_net::sha256_hex(data)
}
