//! Hashes of files on disk.

use std::fmt::Write as _;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use sha1::Sha1;
use sha2::{Digest, Sha512};

/// The most bytes hashed from one file; a larger file is not a mod, pack or shader.
pub const MAX_HASHED_BYTES: u64 = 1 << 30;

/// The digests the platforms look files up by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHashes {
    /// Lowercase hex SHA-1.
    pub sha1: String,
    /// Lowercase hex SHA-512.
    pub sha512: String,
    /// Size in bytes.
    pub size: u64,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Hashes the file at `path` in one pass.
pub fn hash_file(path: &Path) -> std::io::Result<FileHashes> {
    let mut file = File::open(path)?;
    let mut sha1 = Sha1::new();
    let mut sha512 = Sha512::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        if size > MAX_HASHED_BYTES {
            return Err(std::io::Error::other("file is too large to hash"));
        }
        let chunk = &buf[..n];
        sha1.update(chunk);
        sha512.update(chunk);
    }
    Ok(FileHashes {
        sha1: hex(&sha1.finalize()),
        sha512: hex(&sha512.finalize()),
        size,
    })
}

/// The SHA-1 and size of everything `reader` yields.
pub fn sha1_of(mut reader: impl Read) -> std::io::Result<(String, u64)> {
    let mut sha1 = Sha1::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        sha1.update(&buf[..n]);
    }
    Ok((hex(&sha1.finalize()), size))
}
