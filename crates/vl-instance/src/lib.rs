//! Instances on disk.
//!
//! Layout under `<data>/instances/`:
//!
//! - `<id>/instance.json` — the config ([`InstanceConfig`]: name, icon, group, notes, play time,
//!   components); the id is the directory name;
//! - `<id>/minecraft/` — the game directory;
//! - `.trash/` — deleted instances, recoverable by moving a directory back;
//! - `.creating-*` — an instance being created (removed by the next create).
//!
//! Creation builds the instance in a hidden directory and renames it into place, so a crash never
//! leaves a half-made instance in the list. Configs are written atomically. A directory whose
//! config is missing or damaged is listed with a problem and never breaks the list. Delete moves
//! the directory into `.trash/` (never removes data); [`Instances::purge_trash`] empties it.

mod config;
mod naming;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub use config::{
    CONFIG_FORMAT, Component, FABRIC_UID, FORGE_UID, GAME_UID, InstanceConfig, NEOFORGE_UID,
    QUILT_UID,
};
use vl_core::instance::{InstanceState, InstanceSummary, Loader, NewInstance};
use vl_core::{Error, ErrorKind, InstanceId};
use vl_store::write_atomic;

/// File name of the instance config.
pub const CONFIG_FILE: &str = "instance.json";
/// Name of the game directory inside an instance.
pub const GAME_DIR: &str = "minecraft";
const TRASH_DIR: &str = ".trash";
const CREATING_PREFIX: &str = ".creating-";

/// The instances under one root directory.
#[derive(Debug, Clone)]
pub struct Instances {
    root: PathBuf,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn io_err(path: &Path, e: std::io::Error) -> Error {
    Error::new(ErrorKind::Io, format!("{}: {e}", path.display()))
}

impl Instances {
    /// The instances stored in `root` (usually `<data>/instances`); created when missing.
    pub fn open(root: &Path) -> vl_core::Result<Self> {
        fs::create_dir_all(root).map_err(|e| io_err(root, e))?;
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    /// The instances root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory of an instance.
    pub fn dir(&self, id: &InstanceId) -> vl_core::Result<PathBuf> {
        if !naming::is_plain_name(id.as_str()) {
            return Err(Error::invalid_input(format!(
                "invalid instance id {:?}",
                id.as_str()
            )));
        }
        Ok(self.root.join(id.as_str()))
    }

    /// The game directory of an instance.
    pub fn game_dir(&self, id: &InstanceId) -> vl_core::Result<PathBuf> {
        Ok(self.dir(id)?.join(GAME_DIR))
    }

    /// Reads one instance's config.
    pub fn load(&self, id: &InstanceId) -> vl_core::Result<InstanceConfig> {
        let dir = self.dir(id)?;
        if !dir.is_dir() {
            return Err(Error::not_found(format!("instance {id}")));
        }
        read_config(&dir.join(CONFIG_FILE))
    }

    /// Lists all instances, sorted by id. Only the config files are read. A damaged instance is
    /// listed with `problem` set; the others are unaffected.
    pub fn list(&self) -> vl_core::Result<Vec<InstanceSummary>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(&self.root).map_err(|e| io_err(&self.root, e))? {
            let entry = entry.map_err(|e| io_err(&self.root, e))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || !entry.path().is_dir() {
                continue;
            }
            names.push(name);
        }
        names.sort();
        Ok(names
            .into_iter()
            .filter_map(|name| {
                let id = InstanceId::new(&name)?;
                Some(
                    match read_config(&self.root.join(&name).join(CONFIG_FILE)) {
                        Ok(cfg) => summary(&id, &cfg),
                        Err(e) => damaged(id, e.message),
                    },
                )
            })
            .collect())
    }

    /// Creates a vanilla instance.
    pub fn create(&self, new: &NewInstance) -> vl_core::Result<InstanceSummary> {
        let name = new.name.trim();
        if name.is_empty() {
            return Err(Error::invalid_input("the instance name is empty"));
        }
        let version = new.game_version.trim();
        if version.is_empty() {
            return Err(Error::invalid_input("the game version is empty"));
        }
        self.clean_leftovers();
        let cfg = InstanceConfig::vanilla(name, version, new.group.clone(), now());
        let (id, target) = self.free_name(&naming::dir_name(name));
        let staging = self.root.join(format!("{CREATING_PREFIX}{id}"));
        let _ = fs::remove_dir_all(&staging);
        let built = (|| {
            fs::create_dir_all(staging.join(GAME_DIR)).map_err(|e| io_err(&staging, e))?;
            write_config(&staging.join(CONFIG_FILE), &cfg)?;
            fs::rename(&staging, &target).map_err(|e| io_err(&target, e))
        })();
        if let Err(e) = built {
            let _ = fs::remove_dir_all(&staging);
            return Err(e);
        }
        let id = InstanceId::new(&id).ok_or_else(|| Error::internal("empty instance id"))?;
        Ok(summary(&id, &cfg))
    }

    /// Renames an instance and its directory; the id changes with the directory.
    ///
    /// The directory is renamed first and the config written second; if the config write fails
    /// the directory is renamed back, so a failure leaves the old instance intact.
    pub fn rename(&self, id: &InstanceId, new_name: &str) -> vl_core::Result<InstanceSummary> {
        let new_name = new_name.trim();
        if new_name.is_empty() {
            return Err(Error::invalid_input("the instance name is empty"));
        }
        let old_dir = self.dir(id)?;
        let old_cfg = self.load(id)?;
        let mut cfg = old_cfg.clone();
        cfg.name = new_name.to_owned();
        let base = naming::dir_name(new_name);
        if base == id.as_str() {
            write_config(&old_dir.join(CONFIG_FILE), &cfg)?;
            return Ok(summary(id, &cfg));
        }
        let (new_id, new_dir) = self.free_name(&base);
        fs::rename(&old_dir, &new_dir).map_err(|e| io_err(&new_dir, e))?;
        if let Err(e) = write_config(&new_dir.join(CONFIG_FILE), &cfg) {
            let _ = fs::rename(&new_dir, &old_dir);
            return Err(e);
        }
        let new_id = InstanceId::new(&new_id).ok_or_else(|| Error::internal("empty id"))?;
        Ok(summary(&new_id, &cfg))
    }

    /// Moves an instance into `.trash/`; nothing is erased. Returns where it went.
    pub fn delete(&self, id: &InstanceId) -> vl_core::Result<PathBuf> {
        let dir = self.dir(id)?;
        if !dir.is_dir() {
            return Err(Error::not_found(format!("instance {id}")));
        }
        let trash = self.root.join(TRASH_DIR);
        fs::create_dir_all(&trash).map_err(|e| io_err(&trash, e))?;
        let stamp = now();
        let mut n = 0u32;
        loop {
            let name = if n == 0 {
                format!("{id}-{stamp}")
            } else {
                format!("{id}-{stamp}-{n}")
            };
            let target = trash.join(name);
            if !target.exists() {
                fs::rename(&dir, &target).map_err(|e| io_err(&target, e))?;
                return Ok(target);
            }
            n += 1;
        }
    }

    /// Permanently erases everything in `.trash/`.
    pub fn purge_trash(&self) -> vl_core::Result<()> {
        let trash = self.root.join(TRASH_DIR);
        if trash.exists() {
            fs::remove_dir_all(&trash).map_err(|e| io_err(&trash, e))?;
        }
        Ok(())
    }

    /// Records a play session: adds to the play time and sets the last played / last active
    /// times.
    pub fn record_play(&self, id: &InstanceId, secs: u64) -> vl_core::Result<InstanceSummary> {
        let mut cfg = self.load(id)?;
        let t = now();
        cfg.play_time_secs = cfg.play_time_secs.saturating_add(secs);
        cfg.last_played_unix = Some(t);
        cfg.last_active_unix = Some(t);
        write_config(&self.dir(id)?.join(CONFIG_FILE), &cfg)?;
        Ok(summary(id, &cfg))
    }

    fn free_name(&self, base: &str) -> (String, PathBuf) {
        let mut n = 1u32;
        loop {
            let name = if n == 1 {
                base.to_owned()
            } else {
                format!("{base}-{n}")
            };
            let path = self.root.join(&name);
            if !path.exists() {
                return (name, path);
            }
            n += 1;
        }
    }

    fn clean_leftovers(&self) {
        if let Ok(rd) = fs::read_dir(&self.root) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with(CREATING_PREFIX) {
                    let _ = fs::remove_dir_all(e.path());
                }
            }
        }
    }
}

fn read_config(path: &Path) -> vl_core::Result<InstanceConfig> {
    let bytes = fs::read(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::new(ErrorKind::Integrity, format!("missing {CONFIG_FILE}"))
        } else {
            io_err(path, e)
        }
    })?;
    let cfg: InstanceConfig = serde_json::from_slice(&bytes)
        .map_err(|e| Error::new(ErrorKind::Integrity, format!("damaged {CONFIG_FILE}: {e}")))?;
    if cfg.format > CONFIG_FORMAT {
        return Err(Error::unsupported(format!(
            "{CONFIG_FILE} format {} is newer than the supported {CONFIG_FORMAT}",
            cfg.format
        )));
    }
    if cfg.game_version().is_none() {
        return Err(Error::new(
            ErrorKind::Integrity,
            "the config has no game version",
        ));
    }
    Ok(cfg)
}

fn write_config(path: &Path, cfg: &InstanceConfig) -> vl_core::Result<()> {
    let bytes = serde_json::to_vec_pretty(cfg).map_err(|e| Error::internal(e.to_string()))?;
    write_atomic(path, &bytes)?;
    Ok(())
}

fn summary(id: &InstanceId, cfg: &InstanceConfig) -> InstanceSummary {
    InstanceSummary {
        id: id.clone(),
        name: cfg.name.clone(),
        group: cfg.group.clone(),
        game_version: cfg.game_version().unwrap_or_default().to_owned(),
        loader: cfg.loader(),
        play_time_secs: cfg.play_time_secs,
        last_played_unix: cfg.last_played_unix,
        state: InstanceState::Stopped,
        problem: None,
    }
}

fn damaged(id: InstanceId, problem: String) -> InstanceSummary {
    InstanceSummary {
        name: id.to_string(),
        id,
        group: None,
        game_version: String::new(),
        loader: Loader::Vanilla,
        play_time_secs: 0,
        last_played_unix: None,
        state: InstanceState::Stopped,
        problem: Some(problem),
    }
}

#[cfg(test)]
mod tests;
