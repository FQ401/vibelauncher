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

/// A tar entry: its raw name, type, mode and content (a link's target for links).
pub(crate) struct TarEntry<'a> {
    pub(crate) name: &'a str,
    pub(crate) kind: tar::EntryType,
    pub(crate) mode: u32,
    pub(crate) content: &'a [u8],
}

impl<'a> TarEntry<'a> {
    pub(crate) fn file(name: &'a str, content: &'a [u8]) -> Self {
        Self {
            name,
            kind: tar::EntryType::Regular,
            mode: 0o644,
            content,
        }
    }

    pub(crate) fn exe(name: &'a str, content: &'a [u8]) -> Self {
        Self {
            mode: 0o755,
            ..Self::file(name, content)
        }
    }

    pub(crate) fn dir(name: &'a str) -> Self {
        Self {
            name,
            kind: tar::EntryType::Directory,
            mode: 0o755,
            content: b"",
        }
    }
}

/// A gzip-compressed tar archive of `entries`. Names are written as they are, unchecked, so
/// tests can build unsafe archives.
pub(crate) fn tar_gz_of(entries: &[TarEntry<'_>]) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut tar = tar::Builder::new(gz);
    for entry in entries {
        let mut header = tar::Header::new_gnu();
        let name = entry.name.as_bytes();
        header.as_old_mut().name[..name.len()].copy_from_slice(name);
        header.set_entry_type(entry.kind);
        header.set_mode(entry.mode);
        header.set_mtime(0);
        let link = matches!(entry.kind, tar::EntryType::Symlink | tar::EntryType::Link);
        if link {
            let target = entry.content;
            header.as_old_mut().linkname[..target.len()].copy_from_slice(target);
            header.set_size(0);
        } else {
            header.set_size(entry.content.len() as u64);
        }
        header.set_cksum();
        let data: &[u8] = if link { b"" } else { entry.content };
        tar.append(&header, data).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap()
}

/// The lowercase hex SHA-256 of `data`.
pub(crate) fn sha256(data: &[u8]) -> String {
    vl_net::sha256_hex(data)
}
