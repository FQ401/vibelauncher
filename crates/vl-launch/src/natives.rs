//! Unpacking native libraries.

use std::fs;
use std::io::{self, BufReader, Read, Write};
use std::path::Path;

use vl_core::{Error, ErrorKind, Result};
use vl_meta::libraries::NativeArchive;
use zip::ZipArchive;

use crate::job::Job;
use crate::layout::join_relative;

const MARKER: &str = ".unpacked";
/// Limits against archives that unpack into something much larger than they are.
const MAX_ENTRIES: usize = 4096;
const MAX_ENTRY_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;

fn io_error(path: &Path, e: &io::Error) -> Error {
    Error::new(ErrorKind::Io, format!("{}: {e}", path.display()))
}

fn damaged(path: &Path, what: impl std::fmt::Display) -> Error {
    Error::new(
        ErrorKind::Integrity,
        format!("native archive {}: {what}", path.display()),
    )
}

/// What the unpacked directory must correspond to; when it matches the marker file, unpacking is
/// skipped.
fn signature(archives: &[NativeArchive], libraries_dir: &Path) -> String {
    let mut out = String::new();
    for a in archives {
        let size = fs::metadata(join_relative(libraries_dir, &a.file.path)).map_or(0, |m| m.len());
        out.push_str(&format!("{}:{size}:{}\n", a.file.path, a.exclude.join(",")));
    }
    out
}

/// Unpacks `archives` (paths below `libraries_dir`) into `dest`, honouring each archive's
/// `extract.exclude`. Entry names are checked by `vl-meta`; sizes and counts are capped.
pub(crate) fn extract_all(
    archives: &[NativeArchive],
    libraries_dir: &Path,
    dest: &Path,
    job: &Job,
) -> Result<()> {
    let wanted = signature(archives, libraries_dir);
    let marker = dest.join(MARKER);
    if fs::read_to_string(&marker).is_ok_and(|m| m == wanted) {
        return Ok(());
    }
    fs::create_dir_all(dest).map_err(|e| io_error(dest, &e))?;
    let mut total = 0u64;
    for archive in archives {
        job.check()?;
        let path = join_relative(libraries_dir, &archive.file.path);
        let file = fs::File::open(&path).map_err(|e| io_error(&path, &e))?;
        let mut zip = ZipArchive::new(BufReader::new(file)).map_err(|e| damaged(&path, e))?;
        if zip.len() > MAX_ENTRIES {
            return Err(damaged(&path, "too many entries"));
        }
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).map_err(|e| damaged(&path, e))?;
            let name = entry.name().map_err(|e| damaged(&path, e))?.into_owned();
            let Some(target) = archive.target(&name) else {
                continue;
            };
            if entry.size() > MAX_ENTRY_BYTES {
                return Err(damaged(&path, "an entry is too large"));
            }
            let out = join_relative(dest, &target);
            if fs::metadata(&out).is_ok_and(|m| m.len() == entry.size()) {
                total = total.saturating_add(entry.size());
                continue;
            }
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent).map_err(|e| io_error(parent, &e))?;
            }
            let temp = out.with_extension("vl-part");
            let mut file = fs::File::create(&temp).map_err(|e| io_error(&temp, &e))?;
            let written = io::copy(&mut (&mut entry).take(MAX_ENTRY_BYTES + 1), &mut file)
                .map_err(|e| io_error(&temp, &e))?;
            file.flush().map_err(|e| io_error(&temp, &e))?;
            drop(file);
            total = total.saturating_add(written);
            if written > MAX_ENTRY_BYTES || total > MAX_TOTAL_BYTES {
                let _ = fs::remove_file(&temp);
                return Err(damaged(&path, "unpacks to too much data"));
            }
            fs::rename(&temp, &out).map_err(|e| io_error(&out, &e))?;
        }
    }
    fs::write(&marker, wanted).map_err(|e| io_error(&marker, &e))?;
    Ok(())
}
