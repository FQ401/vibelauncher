//! The worlds of an instance (`saves/*`) and what `level.dat` says about them.
//!
//! A world is a folder of `saves/` that holds a `level.dat`. `level.dat` is a gzip-compressed
//! NBT file written by the game; only a few fields are read, with caps on size and depth, and a
//! file that cannot be read leaves the fields empty.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use flate2::read::GzDecoder;
use vl_core::resources::WorldInfo;
use vl_core::{Error, ErrorKind};

/// The folder of the worlds, relative to the game directory.
pub const SAVES_DIR: &str = "saves";
/// Most worlds listed.
const MAX_WORLDS: usize = 5_000;
/// Most decompressed bytes of `level.dat` that are read.
const MAX_NBT_BYTES: u64 = 16 * 1024 * 1024;
/// Deepest NBT nesting followed.
const MAX_DEPTH: usize = 32;

/// A world folder name: one path segment that cannot leave `saves/`.
///
/// Names may hold spaces and any script; they cannot be empty, `.`, `..`, hold a path separator,
/// a drive colon or control characters, or end in a dot or space (which Windows would drop).
pub(crate) fn check_world_name(name: &str) -> vl_core::Result<()> {
    let bad = name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', ':'])
        || name.chars().any(char::is_control)
        || name.ends_with(['.', ' ']);
    if bad {
        return Err(Error::invalid_input(format!("invalid world name {name:?}")));
    }
    Ok(())
}

/// The folder of the world `name` under `game_dir`.
///
/// # Errors
///
/// `InvalidInput` for a name that is not a plain folder name; `NotFound` when there is no such
/// world (a real folder with a `level.dat`; a link is not followed).
pub(crate) fn world_dir(game_dir: &Path, name: &str) -> vl_core::Result<PathBuf> {
    check_world_name(name)?;
    let dir = game_dir.join(SAVES_DIR).join(name);
    let is_world =
        fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) && dir.join("level.dat").is_file();
    if is_world {
        Ok(dir)
    } else {
        Err(Error::new(
            ErrorKind::NotFound,
            format!("world {name:?} (create a world first, or check the name)"),
        ))
    }
}

/// The worlds under `game_dir`, sorted by name; a missing `saves/` is no worlds.
pub(crate) fn list_worlds(game_dir: &Path) -> vl_core::Result<Vec<WorldInfo>> {
    let saves = game_dir.join(SAVES_DIR);
    let read = match fs::read_dir(&saves) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(Error::new(
                ErrorKind::Io,
                format!("{}: {e}", saves.display()),
            ));
        }
    };
    let mut worlds = Vec::new();
    for item in read.flatten() {
        let name = item.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || check_world_name(&name).is_err() {
            continue;
        }
        let Ok(dir) = world_dir(game_dir, &name) else {
            continue;
        };
        let level = read_level(&dir.join("level.dat"));
        let last_played = level.last_played.or_else(|| {
            fs::metadata(dir.join("level.dat"))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .and_then(|d| i64::try_from(d.as_secs()).ok())
        });
        let datapacks = fs::read_dir(dir.join("datapacks")).map_or(0, |r| {
            let count = r
                .flatten()
                .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                .count();
            u32::try_from(count).unwrap_or(u32::MAX)
        });
        worlds.push(WorldInfo {
            display_name: level.name.filter(|n| *n != name),
            version: level.version,
            last_played,
            datapacks,
            name,
        });
        if worlds.len() >= MAX_WORLDS {
            break;
        }
    }
    worlds.sort_by_key(|w| (w.name.to_lowercase(), w.name.clone()));
    Ok(worlds)
}

/// The fields of `level.dat` that are shown.
#[derive(Debug, Default, PartialEq, Eq)]
struct Level {
    name: Option<String>,
    version: Option<String>,
    /// Seconds since the Unix epoch.
    last_played: Option<i64>,
}

fn read_level(path: &Path) -> Level {
    let Ok(file) = fs::File::open(path) else {
        return Level::default();
    };
    let mut bytes = Vec::new();
    if GzDecoder::new(file)
        .take(MAX_NBT_BYTES)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return Level::default();
    }
    parse_level(&bytes)
}

fn parse_level(bytes: &[u8]) -> Level {
    let mut reader = Nbt { bytes, at: 0 };
    let Some(Value::Compound(root)) = reader.root() else {
        return Level::default();
    };
    let Some(Value::Compound(data)) = find(&root, "Data") else {
        return Level::default();
    };
    let text = |fields: &[(String, Value)], key: &str| match find(fields, key) {
        Some(Value::Text(t)) if !t.is_empty() => Some(t.clone()),
        _ => None,
    };
    Level {
        name: text(data, "LevelName"),
        version: match find(data, "Version") {
            Some(Value::Compound(v)) => text(v, "Name"),
            _ => None,
        },
        last_played: match find(data, "LastPlayed") {
            Some(Value::Number(ms)) if *ms > 0 => Some(ms / 1000),
            _ => None,
        },
    }
}

fn find<'a>(fields: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
    fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// The NBT values that matter here; everything else is skipped.
#[derive(Debug)]
enum Value {
    Text(String),
    Number(i64),
    Compound(Vec<(String, Value)>),
    Other,
}

struct Nbt<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Nbt<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let end = self.at.checked_add(n)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    fn i32(&mut self) -> Option<i32> {
        self.take(4)
            .map(|b| i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn string(&mut self) -> Option<String> {
        let len = self
            .take(2)
            .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))?;
        // Modified UTF-8 matches UTF-8 for everything but NUL and supplementary characters; a
        // lossy conversion is enough for display.
        self.take(len)
            .map(|b| String::from_utf8_lossy(b).into_owned())
    }

    fn root(&mut self) -> Option<Value> {
        if self.u8()? != 10 {
            return None;
        }
        self.string()?;
        self.payload(10, 0)
    }

    fn payload(&mut self, tag: u8, depth: usize) -> Option<Value> {
        if depth > MAX_DEPTH {
            return None;
        }
        match tag {
            1 => self.take(1).map(|b| Value::Number(i64::from(b[0] as i8))),
            2 => self
                .take(2)
                .map(|b| Value::Number(i64::from(i16::from_be_bytes([b[0], b[1]])))),
            3 => self.i32().map(|n| Value::Number(i64::from(n))),
            4 => self.take(8).map(|b| {
                let mut a = [0u8; 8];
                a.copy_from_slice(b);
                Value::Number(i64::from_be_bytes(a))
            }),
            5 => self.take(4).map(|_| Value::Other),
            6 => self.take(8).map(|_| Value::Other),
            7 => self.array(1),
            8 => self.string().map(Value::Text),
            9 => {
                let item = self.u8()?;
                let len = usize::try_from(self.i32()?).ok()?;
                for _ in 0..len {
                    if item == 0 {
                        break;
                    }
                    self.payload(item, depth + 1)?;
                }
                Some(Value::Other)
            }
            10 => {
                let mut fields = Vec::new();
                loop {
                    let tag = self.u8()?;
                    if tag == 0 {
                        return Some(Value::Compound(fields));
                    }
                    let name = self.string()?;
                    let value = self.payload(tag, depth + 1)?;
                    fields.push((name, value));
                }
            }
            11 => self.array(4),
            12 => self.array(8),
            _ => None,
        }
    }

    /// An array of `size`-byte elements, skipped.
    fn array(&mut self, size: usize) -> Option<Value> {
        let len = usize::try_from(self.i32()?).ok()?;
        self.take(len.checked_mul(size)?).map(|_| Value::Other)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn string(out: &mut Vec<u8>, s: &str) {
        out.extend_from_slice(&u16::try_from(s.len()).unwrap().to_be_bytes());
        out.extend_from_slice(s.as_bytes());
    }

    /// `Data { LevelName, LastPlayed, Skipped: [1, 2], Version { Name } }`.
    pub(crate) fn level_nbt(name: &str, version: &str, last_played_ms: i64) -> Vec<u8> {
        let mut b = vec![10];
        string(&mut b, "");
        b.push(10);
        string(&mut b, "Data");
        b.push(8);
        string(&mut b, "LevelName");
        string(&mut b, name);
        b.push(4);
        string(&mut b, "LastPlayed");
        b.extend_from_slice(&last_played_ms.to_be_bytes());
        b.push(9);
        string(&mut b, "Skipped");
        b.push(3);
        b.extend_from_slice(&2i32.to_be_bytes());
        b.extend_from_slice(&1i32.to_be_bytes());
        b.extend_from_slice(&2i32.to_be_bytes());
        b.push(10);
        string(&mut b, "Version");
        b.push(8);
        string(&mut b, "Name");
        string(&mut b, version);
        b.push(0);
        b.push(0);
        b.push(0);
        b
    }

    #[test]
    fn reads_the_shown_fields() {
        let level = parse_level(&level_nbt(
            "\u{41c}\u{438}\u{440} 1",
            "1.21.4",
            1_700_000_000_000,
        ));
        assert_eq!(level.name.as_deref(), Some("\u{41c}\u{438}\u{440} 1"));
        assert_eq!(level.version.as_deref(), Some("1.21.4"));
        assert_eq!(level.last_played, Some(1_700_000_000));
    }

    #[test]
    fn damaged_nbt_gives_nothing() {
        let good = level_nbt("a", "1", 5000);
        for cut in 0..good.len() {
            let _ = parse_level(&good[..cut]);
        }
        assert_eq!(parse_level(b"stub"), Level::default());
        assert_eq!(parse_level(&[]), Level::default());
    }

    #[test]
    fn names_cannot_leave_saves() {
        for bad in [
            "", ".", "..", "a/b", "a\\b", "C:x", "../x", "a\0b", "x.", "x ", "..\\..",
        ] {
            assert!(check_world_name(bad).is_err(), "{bad:?}");
        }
        for good in [
            "Test World",
            "\u{41c}\u{438}\u{440} 1",
            "\u{4e16}\u{754c}",
            "a.b",
            "new world (2)",
        ] {
            assert!(check_world_name(good).is_ok(), "{good:?}");
        }
    }
}
