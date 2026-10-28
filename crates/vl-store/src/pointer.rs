//! The pointer file: a small file in the OS folder for settings that names the data folder when
//! it is not the OS convention. It is the only thing the launcher keeps there.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{StoreError, write_atomic};

/// The pointer's file name.
pub const POINTER_FILE: &str = "root.json";

/// The pointer's content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pointer {
    /// The data folder.
    pub root: PathBuf,
    /// The data folder before the last move, for diagnostics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<PathBuf>,
}

/// Reads the pointer; `None` when there is none. A damaged pointer is an error: guessing another
/// folder would show the user an empty launcher.
pub fn read(file: &Path) -> Result<Option<Pointer>, StoreError> {
    let bytes = match fs::read(file) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(StoreError::io(file, e)),
    };
    let pointer: Pointer = serde_json::from_slice(&bytes)
        .map_err(|e| StoreError::corrupt(file, format!("damaged pointer file: {e}")))?;
    if !pointer.root.is_absolute() {
        return Err(StoreError::corrupt(
            file,
            "the pointer names a relative path",
        ));
    }
    Ok(Some(pointer))
}

/// Writes the pointer atomically.
pub fn write(file: &Path, root: &Path, previous: Option<&Path>) -> Result<(), StoreError> {
    let pointer = Pointer {
        root: root.to_path_buf(),
        previous: previous.map(Path::to_path_buf),
    };
    let text = serde_json::to_vec_pretty(&pointer)
        .map_err(|e| StoreError::corrupt(file, e.to_string()))?;
    write_atomic(file, &text)
}

/// Removes the pointer, so the OS convention applies again.
pub fn remove(file: &Path) -> Result<(), StoreError> {
    match fs::remove_file(file) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(StoreError::io(file, e)),
    }
}
