//! The launcher's own trash of deleted instances.
//!
//! Layout under the trash folder (`<data>/trash`):
//!
//! - `<item>/` — a deleted instance folder as it was (config, game directory, its own resource
//!   trash); the item id is a plain folder name, the instance id with a suffix when taken;
//! - `<item>.json` — its record ([`Meta`]): the instance id, name, group, game version, loader,
//!   deletion time, size and the user's comment;
//! - `.incoming-<item>/` — a copy on its way in across volumes;
//! - `.restoring-<item>/` — an item on its way out across volumes;
//! - `.removing-*` — something being erased.
//!
//! **Moves.** Delete renames the instance folder into the trash and restore renames it back. When
//! the trash and the instances are on different volumes (a section moved to another drive) a
//! rename is impossible, and the folder is moved by copy instead:
//!
//! 1. the source is renamed next to itself (`.deleting-<item>` in the instances folder, or
//!    `.restoring-<item>` in the trash), so it leaves the list at once;
//! 2. it is copied into a staging folder on the other volume (`.incoming-<item>` in the trash,
//!    or `.restoring-<id>` in the instances folder), each file flushed to disk;
//! 3. the copy is checked against the source: the same folders and files with the same sizes;
//! 4. the staging folder is renamed to its final name — the commit;
//! 5. the parked source is erased.
//!
//! A failure before the commit removes the copy and renames the source back. [`Trash::recover`],
//! run when the launcher starts, finishes what a crash interrupted: a parked source whose
//! destination exists was committed and is erased; any other parked source goes back; a staging
//! folder is never the only copy and is erased. Links inside an instance are not followed, not
//! measured and not copied (a move across volumes leaves them out); a rename keeps them.
//!
//! The record is written before the folder moves. A record without a folder is removed by
//! [`Trash::recover`]; a folder without a readable record is listed with a problem and can be
//! restored or purged like any other item. Ids that come from a record are checked to be plain
//! folder names before they are used as paths.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use vl_core::instance::{InstanceSummary, Loader};
use vl_core::trash::{COMMENT_MAX_CHARS, PurgeReport, TrashItem};
use vl_core::{Error, ErrorKind, InstanceId};
use vl_store::retention::size_of;
use vl_store::write_atomic;

use crate::{CONFIG_FILE, Instances, damaged, io_err, naming, read_config, summary, write_config};

/// The record of an item: `<item>` + this suffix.
const META_SUFFIX: &str = ".json";
/// The record format this launcher writes and reads.
const META_FORMAT: u32 = 1;
/// A record larger than this is damaged.
const META_MAX_BYTES: u64 = 64 * 1024;
/// A source parked in the instances folder while it is copied into the trash.
const PARK_DELETE: &str = ".deleting-";
/// A copy on its way into the trash.
const STAGE_INCOMING: &str = ".incoming-";
/// A source parked in the trash while it is copied back (in the trash), and the copy on its
/// way back (in the instances folder).
const RESTORING: &str = ".restoring-";
/// Something being erased, in the trash or the instances folder.
const REMOVING: &str = ".removing-";
/// The trash of earlier launcher versions inside the instances folder.
const LEGACY_DIR: &str = ".trash";
const DAY_SECS: i64 = 24 * 60 * 60;

/// The current time in Unix seconds; tests pass a fake one.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// The record kept next to a deleted instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Meta {
    format: u32,
    /// The instance id it had.
    instance: String,
    name: String,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    game_version: String,
    #[serde(default = "vanilla")]
    loader: Loader,
    deleted_unix: i64,
    size: u64,
    #[serde(default)]
    comment: String,
    /// The instance id a restore across volumes is putting it back as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    restoring_to: Option<String>,
}

fn vanilla() -> Loader {
    Loader::Vanilla
}

/// The trash of deleted instances. Cheap to clone; clones share one lock, so trash operations
/// run one at a time (erasing a folder runs after the lock is released).
#[derive(Clone)]
pub struct Trash {
    root: PathBuf,
    instances: Instances,
    clock: Clock,
    lock: Arc<Mutex<()>>,
    /// Takes the copy path as if the trash were on another volume (tests).
    always_copy: bool,
}

impl std::fmt::Debug for Trash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Trash")
            .field("root", &self.root)
            .field("instances", &self.instances.root())
            .finish_non_exhaustive()
    }
}

/// What a deletion into the trash did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deleted {
    /// The new trash item.
    pub item: TrashItem,
    /// Links left out because the instance was copied to another volume.
    pub skipped_links: Vec<PathBuf>,
}

impl Trash {
    /// The trash in `root` (usually `<data>/trash`) for the instances of `instances`. Nothing is
    /// created until the first deletion.
    pub fn new(root: &Path, instances: Instances) -> Self {
        Self {
            root: root.to_path_buf(),
            instances,
            clock: Arc::new(crate::now),
            lock: Arc::new(Mutex::new(())),
            always_copy: false,
        }
    }

    /// The same trash with another clock.
    #[must_use]
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// The trash folder.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether a trash operation is running now.
    pub fn is_busy(&self) -> bool {
        matches!(self.lock.try_lock(), Err(TryLockError::WouldBlock))
    }

    fn guard(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn meta_path(&self, item: &str) -> PathBuf {
        self.root.join(format!("{item}{META_SUFFIX}"))
    }

    /// The folder of an existing item; the id must be a plain folder name.
    fn item_dir(&self, item: &str) -> vl_core::Result<PathBuf> {
        if !naming::is_plain_name(item) {
            return Err(Error::invalid_input(format!("invalid trash item {item:?}")));
        }
        let dir = self.root.join(item);
        match fs::symlink_metadata(&dir) {
            Ok(m) if m.is_dir() => Ok(dir),
            _ => Err(Error::not_found(format!("trash item {item}"))),
        }
    }

    /// Moves an instance into the trash with `comment`.
    ///
    /// # Errors
    ///
    /// `InvalidInput` for a bad id or a comment longer than [`COMMENT_MAX_CHARS`], `NotFound`
    /// for a missing instance, `Io` when it cannot be moved (nothing changes then).
    pub fn delete(&self, id: &InstanceId, comment: &str) -> vl_core::Result<Deleted> {
        let comment = comment.trim();
        if comment.chars().count() > COMMENT_MAX_CHARS {
            return Err(Error::invalid_input(format!(
                "the comment is longer than {COMMENT_MAX_CHARS} characters"
            )));
        }
        let _guard = self.guard();
        let dir = self.instance_dir(id)?;
        let (name, group, game_version, loader) = match read_config(&dir.join(CONFIG_FILE)) {
            Ok(cfg) => {
                let game = cfg.game_version().unwrap_or_default().to_owned();
                let loader = cfg.loader();
                (cfg.name, cfg.group, game, loader)
            }
            Err(_) => (id.to_string(), None, String::new(), Loader::Vanilla),
        };
        fs::create_dir_all(&self.root).map_err(|e| io_err(&self.root, e))?;
        let item = self.free_item(id.as_str());
        let meta = Meta {
            format: META_FORMAT,
            instance: id.to_string(),
            name,
            group,
            game_version,
            loader,
            deleted_unix: (self.clock)(),
            size: size_of(&dir),
            comment: comment.to_owned(),
            restoring_to: None,
        };
        self.write_meta(&item, &meta)?;
        let moved = self.move_dir(
            &dir,
            &self.root.join(&item),
            &self.instances.root().join(format!("{PARK_DELETE}{item}")),
            &self.root.join(format!("{STAGE_INCOMING}{item}")),
        );
        match moved {
            Ok(skipped_links) => Ok(Deleted {
                item: item_of(&item, &meta),
                skipped_links,
            }),
            Err(e) => {
                let _ = fs::remove_file(self.meta_path(&item));
                Err(e)
            }
        }
    }

    /// Erases an instance at once, bypassing the trash. The folder leaves the list with one
    /// rename; what cannot be erased now (a file another program holds open) is erased by the
    /// next [`Trash::collect_garbage`].
    ///
    /// # Errors
    ///
    /// `InvalidInput` for a bad id, `NotFound` for a missing instance, `Io` when it cannot be
    /// renamed (nothing changes then).
    pub fn delete_permanently(&self, id: &InstanceId) -> vl_core::Result<()> {
        let removing = {
            let _guard = self.guard();
            let dir = self.instance_dir(id)?;
            let removing = free_hidden(self.instances.root(), REMOVING, id.as_str());
            fs::rename(&dir, &removing).map_err(|e| io_err(&dir, e))?;
            removing
        };
        let _ = remove_tree(&removing);
        Ok(())
    }

    /// The folder of an instance that exists (a link to a folder counts and is never followed).
    fn instance_dir(&self, id: &InstanceId) -> vl_core::Result<PathBuf> {
        let dir = self.instances.dir(id)?;
        match fs::symlink_metadata(&dir) {
            Ok(m) if m.is_dir() || m.file_type().is_symlink() => Ok(dir),
            _ => Err(Error::not_found(format!("instance {id}"))),
        }
    }

    /// The deleted instances, the most recently deleted first.
    ///
    /// # Errors
    ///
    /// `Io` when the trash folder cannot be read; a missing folder is an empty trash.
    pub fn list(&self) -> vl_core::Result<Vec<TrashItem>> {
        let mut items: Vec<TrashItem> = self
            .item_names()?
            .into_iter()
            .map(|name| self.describe(&name))
            .collect();
        items.sort_by(|a, b| {
            b.deleted_unix
                .cmp(&a.deleted_unix)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(items)
    }

    /// The names of the item folders.
    fn item_names(&self) -> vl_core::Result<Vec<String>> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_err(&self.root, e)),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| io_err(&self.root, e))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            if is_dir && naming::is_plain_name(&name) {
                names.push(name);
            }
        }
        names.sort();
        Ok(names)
    }

    /// An item as the list shows it; a missing or damaged record is a problem, not an error.
    fn describe(&self, item: &str) -> TrashItem {
        match self.read_meta(item) {
            Ok(meta) => item_of(item, &meta),
            Err(problem) => {
                let dir = self.root.join(item);
                let config = read_config(&dir.join(CONFIG_FILE)).ok();
                let deleted_unix = fs::symlink_metadata(&dir)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
                TrashItem {
                    id: item.to_owned(),
                    instance: item.to_owned(),
                    name: config
                        .as_ref()
                        .map_or_else(|| item.to_owned(), |c| c.name.clone()),
                    group: config.as_ref().and_then(|c| c.group.clone()),
                    game_version: config
                        .as_ref()
                        .and_then(|c| c.game_version().map(str::to_owned))
                        .unwrap_or_default(),
                    loader: config.as_ref().map_or(Loader::Vanilla, |c| c.loader()),
                    deleted_unix,
                    size: size_of(&dir),
                    comment: String::new(),
                    problem: Some(problem),
                }
            }
        }
    }

    /// Puts a deleted instance back. A taken id gets a suffix (`-2`); a display name another
    /// instance has gets one too (` (2)`).
    ///
    /// # Errors
    ///
    /// `InvalidInput` for a bad item id, `NotFound` for a missing item, `Io` when it cannot be
    /// moved (the item stays in the trash then).
    pub fn restore(&self, item: &str) -> vl_core::Result<InstanceSummary> {
        let _guard = self.guard();
        let src = self.item_dir(item)?;
        let meta = self.read_meta(item).ok();
        let base = meta
            .as_ref()
            .map(|m| m.instance.as_str())
            .filter(|id| naming::is_plain_name(id))
            .unwrap_or(item)
            .to_owned();
        fs::create_dir_all(self.instances.root()).map_err(|e| io_err(self.instances.root(), e))?;
        let (target_id, target) = self.instances.free_name(&base);
        if let Some(meta) = &meta {
            // A restore across volumes that a crash interrupts is finished from this.
            let mut marked = meta.clone();
            marked.restoring_to = Some(target_id.clone());
            self.write_meta(item, &marked)?;
        }
        let moved = self.move_dir(
            &src,
            &target,
            &self.root.join(format!("{RESTORING}{item}")),
            &self
                .instances
                .root()
                .join(format!("{RESTORING}{target_id}")),
        );
        if let Err(e) = moved {
            if let Some(meta) = &meta {
                let _ = self.write_meta(item, meta);
            }
            return Err(e);
        }
        let _ = fs::remove_file(self.meta_path(item));
        let id = InstanceId::new(&target_id).ok_or_else(|| Error::internal("empty instance id"))?;
        Ok(self.settle_name(&id, &target))
    }

    /// Gives a restored instance a display name no other instance has.
    fn settle_name(&self, id: &InstanceId, dir: &Path) -> InstanceSummary {
        let config_path = dir.join(CONFIG_FILE);
        let mut cfg = match read_config(&config_path) {
            Ok(cfg) => cfg,
            Err(e) => return damaged(id.clone(), e.message),
        };
        let taken: Vec<String> = self
            .instances
            .list()
            .unwrap_or_default()
            .into_iter()
            .filter(|i| &i.id != id)
            .map(|i| i.name.to_lowercase())
            .collect();
        if taken.contains(&cfg.name.to_lowercase()) {
            let base = cfg.name.clone();
            let mut n = 2u32;
            while taken.contains(&format!("{base} ({n})").to_lowercase()) {
                n += 1;
            }
            cfg.name = format!("{base} ({n})");
            if write_config(&config_path, &cfg).is_err() {
                cfg.name = base;
            }
        }
        summary(id, &cfg)
    }

    /// Erases one item.
    ///
    /// # Errors
    ///
    /// `InvalidInput` for a bad item id, `NotFound` for a missing item.
    pub fn purge(&self, item: &str) -> vl_core::Result<PurgeReport> {
        let mut report = PurgeReport::default();
        let removing = {
            let _guard = self.guard();
            let dir = self.item_dir(item)?;
            let size = self
                .read_meta(item)
                .map_or_else(|_| size_of(&dir), |m| m.size);
            let removing = free_hidden(&self.root, REMOVING, item);
            fs::rename(&dir, &removing).map_err(|e| io_err(&dir, e))?;
            let _ = fs::remove_file(self.meta_path(item));
            report.removed.push(item.to_owned());
            report.freed = size;
            removing
        };
        let _ = remove_tree(&removing);
        Ok(report)
    }

    /// Erases every item in `items`; an item that cannot be erased is reported, not an error.
    pub fn purge_items(&self, items: &[String]) -> PurgeReport {
        let mut report = PurgeReport::default();
        for item in items {
            match self.purge(item) {
                Ok(one) => {
                    report.removed.extend(one.removed);
                    report.freed += one.freed;
                }
                Err(e) => report.failed.push((item.clone(), e.message)),
            }
        }
        report
    }

    /// Erases every item.
    ///
    /// # Errors
    ///
    /// `Io` when the trash folder cannot be read.
    pub fn purge_all(&self) -> vl_core::Result<PurgeReport> {
        Ok(self.purge_items(&self.item_names()?))
    }

    /// The items beyond the limits: older than `keep_days`, then the oldest while the trash is
    /// larger than `max_bytes`. The most recently deleted item is never chosen for size.
    ///
    /// # Errors
    ///
    /// `Io` when the trash folder cannot be read.
    pub fn due(&self, keep_days: u32, max_bytes: u64) -> vl_core::Result<Vec<String>> {
        let now = (self.clock)();
        let mut items = self.list()?;
        // Oldest first.
        items.reverse();
        let horizon = now.saturating_sub(i64::from(keep_days).saturating_mul(DAY_SECS));
        let (expired, mut kept): (Vec<TrashItem>, Vec<TrashItem>) =
            items.into_iter().partition(|i| i.deleted_unix <= horizon);
        let mut due: Vec<String> = expired.into_iter().map(|i| i.id).collect();
        let mut total: u64 = kept.iter().map(|i| i.size).sum();
        // The newest item stays even when it alone is over the limit.
        kept.pop();
        for item in kept {
            if total <= max_bytes {
                break;
            }
            total = total.saturating_sub(item.size);
            due.push(item.id);
        }
        Ok(due)
    }

    /// Bytes the trash takes, measured (links not followed).
    pub fn size(&self) -> u64 {
        size_of(&self.root)
    }

    /// Whether [`Trash::collect_garbage`] or [`Trash::adopt_legacy`] has work to do.
    pub fn has_garbage(&self) -> bool {
        let hidden = |dir: &Path, prefix: &str| {
            fs::read_dir(dir).is_ok_and(|entries| {
                entries
                    .flatten()
                    .any(|e| e.file_name().to_string_lossy().starts_with(prefix))
            })
        };
        hidden(&self.root, REMOVING)
            || hidden(self.instances.root(), REMOVING)
            || self.instances.root().join(LEGACY_DIR).is_dir()
    }

    /// Finishes what a crash interrupted; only renames and small files, so it is quick. Returns
    /// a line for the launcher log for each thing it did. Run it when the launcher starts,
    /// before other trash work.
    pub fn recover(&self) -> Vec<String> {
        let _guard = self.guard();
        let mut notes = Vec::new();
        let instances = self.instances.root().to_path_buf();
        // Deletions across volumes.
        for (park, item) in hidden_entries(&instances, PARK_DELETE) {
            self.recover_deletion(&park, &item, &self.free_name_for(&item), &mut notes);
        }
        let legacy = instances.join(LEGACY_DIR);
        for (park, item) in hidden_entries(&legacy, PARK_DELETE) {
            let back = free_plain(&legacy, &item);
            self.recover_deletion(&park, &item, &back, &mut notes);
        }
        // Restores across volumes.
        for (park, item) in hidden_entries(&self.root, RESTORING) {
            self.recover_restore(&park, &item, &mut notes);
        }
        // Copies that never reached their commit; the source is still in place.
        let staging = hidden_entries(&self.root, STAGE_INCOMING)
            .into_iter()
            .chain(hidden_entries(&instances, RESTORING));
        for (path, name) in staging {
            let parent = path.parent().map_or_else(PathBuf::new, Path::to_path_buf);
            let removing = free_hidden(&parent, REMOVING, &name);
            if fs::rename(&path, &removing).is_ok() {
                notes.push(format!(
                    "trash: dropped an unfinished copy {}",
                    path.display()
                ));
            }
        }
        // Records without a folder.
        if let Ok(entries) = fs::read_dir(&self.root) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let Some(item) = name.strip_suffix(META_SUFFIX) else {
                    continue;
                };
                let is_file = entry.file_type().is_ok_and(|t| t.is_file());
                if is_file
                    && naming::is_plain_name(item)
                    && fs::symlink_metadata(self.root.join(item)).is_err()
                {
                    let _ = fs::remove_file(entry.path());
                    notes.push(format!(
                        "trash: removed the record of a missing item {item}"
                    ));
                }
            }
        }
        notes
    }

    /// The instance id a parked deletion goes back as.
    fn free_name_for(&self, item: &str) -> PathBuf {
        if !naming::is_plain_name(item) {
            return self.instances.free_name("restored").1;
        }
        let base = self
            .read_meta(item)
            .ok()
            .map(|m| m.instance)
            .filter(|id| naming::is_plain_name(id))
            .unwrap_or_else(|| item.to_owned());
        self.instances.free_name(&base).1
    }

    fn recover_deletion(&self, park: &Path, item: &str, back: &Path, notes: &mut Vec<String>) {
        if naming::is_plain_name(item) && self.root.join(item).is_dir() {
            // Committed: the copy is in the trash, the parked source is only waiting to go.
            let removing = free_hidden(
                park.parent().unwrap_or(self.instances.root()),
                REMOVING,
                item,
            );
            let _ = fs::rename(park, removing);
            notes.push(format!("trash: finished moving {item} into the trash"));
            return;
        }
        let staging = self.root.join(format!("{STAGE_INCOMING}{item}"));
        let _ = remove_tree(&staging);
        if fs::rename(park, back).is_ok() {
            if naming::is_plain_name(item) {
                let _ = fs::remove_file(self.meta_path(item));
            }
            notes.push(format!(
                "trash: an interrupted deletion was undone; the instance is back in {}",
                back.display()
            ));
        }
    }

    fn recover_restore(&self, park: &Path, item: &str, notes: &mut Vec<String>) {
        let meta = if naming::is_plain_name(item) {
            self.read_meta(item).ok()
        } else {
            None
        };
        let target = meta
            .as_ref()
            .and_then(|m| m.restoring_to.clone())
            .filter(|id| naming::is_plain_name(id));
        if let Some(target) = &target {
            let placed = self.instances.root().join(target).is_dir();
            let staging = self.instances.root().join(format!("{RESTORING}{target}"));
            if placed && fs::symlink_metadata(&staging).is_err() {
                // Committed: the instance is back, the parked item is only waiting to go.
                let removing = free_hidden(&self.root, REMOVING, item);
                let _ = fs::rename(park, removing);
                let _ = fs::remove_file(self.meta_path(item));
                notes.push(format!("trash: finished restoring {item} as {target}"));
                return;
            }
            let _ = remove_tree(&staging);
        }
        let back = free_plain(&self.root, item);
        if fs::rename(park, &back).is_ok() {
            if let Some(mut meta) = meta {
                meta.restoring_to = None;
                if let Some(name) = back.file_name().map(|n| n.to_string_lossy().into_owned()) {
                    if name != item {
                        let _ = fs::remove_file(self.meta_path(item));
                    }
                    let _ = self.write_meta(&name, &meta);
                }
            }
            notes.push(format!(
                "trash: an interrupted restore was undone; {item} stays in the trash"
            ));
        }
    }

    /// Erases what earlier operations left to erase (`.removing-*` in the trash and the
    /// instances folder). Returns the bytes freed.
    pub fn collect_garbage(&self) -> u64 {
        let mut freed = 0;
        let instances = self.instances.root().to_path_buf();
        for dir in [
            self.root.clone(),
            instances.clone(),
            instances.join(LEGACY_DIR),
        ] {
            for (path, _) in hidden_entries(&dir, REMOVING) {
                let size = size_of(&path);
                if remove_tree(&path).is_ok() {
                    freed += size;
                }
            }
        }
        freed
    }

    /// Moves the deleted instances of earlier launcher versions (`<instances>/.trash/<id>-<unix
    /// time>`) into this trash, with the time from the name. Returns a line for the launcher log
    /// for each one that could not be moved.
    pub fn adopt_legacy(&self) -> Vec<String> {
        let legacy = self.instances.root().join(LEGACY_DIR);
        let Ok(entries) = fs::read_dir(&legacy) else {
            return Vec::new();
        };
        let mut notes = Vec::new();
        let mut names: Vec<String> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| naming::is_plain_name(n))
            .collect();
        names.sort();
        for name in names {
            if let Err(e) = self.adopt(&legacy, &name) {
                notes.push(format!("trash: kept {name} in {}: {e}", legacy.display()));
            }
        }
        // Only an empty folder is removed.
        let _ = fs::remove_dir(&legacy);
        notes
    }

    fn adopt(&self, legacy: &Path, name: &str) -> vl_core::Result<()> {
        let _guard = self.guard();
        let src = legacy.join(name);
        let (id, stamp) = split_legacy_name(name);
        let deleted_unix = stamp.unwrap_or_else(|| {
            fs::symlink_metadata(&src)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        });
        let config = read_config(&src.join(CONFIG_FILE)).ok();
        fs::create_dir_all(&self.root).map_err(|e| io_err(&self.root, e))?;
        let item = self.free_item(&id);
        let meta = Meta {
            format: META_FORMAT,
            instance: id.clone(),
            name: config
                .as_ref()
                .map_or_else(|| id.clone(), |c| c.name.clone()),
            group: config.as_ref().and_then(|c| c.group.clone()),
            game_version: config
                .as_ref()
                .and_then(|c| c.game_version().map(str::to_owned))
                .unwrap_or_default(),
            loader: config.as_ref().map_or(Loader::Vanilla, |c| c.loader()),
            deleted_unix,
            size: size_of(&src),
            comment: String::new(),
            restoring_to: None,
        };
        self.write_meta(&item, &meta)?;
        let moved = self.move_dir(
            &src,
            &self.root.join(&item),
            &legacy.join(format!("{PARK_DELETE}{item}")),
            &self.root.join(format!("{STAGE_INCOMING}{item}")),
        );
        if let Err(e) = moved {
            let _ = fs::remove_file(self.meta_path(&item));
            return Err(e);
        }
        Ok(())
    }

    /// A free item id made from `base`: `base`, then `base-2`, `base-3`…
    fn free_item(&self, base: &str) -> String {
        let mut n = 1u32;
        loop {
            let name = if n == 1 {
                base.to_owned()
            } else {
                format!("{base}-{n}")
            };
            let taken = [
                self.root.join(&name),
                self.meta_path(&name),
                self.root.join(format!("{STAGE_INCOMING}{name}")),
                self.root.join(format!("{RESTORING}{name}")),
                self.instances.root().join(format!("{PARK_DELETE}{name}")),
            ]
            .iter()
            .any(|p| fs::symlink_metadata(p).is_ok());
            if !taken {
                return name;
            }
            n += 1;
        }
    }

    fn read_meta(&self, item: &str) -> Result<Meta, String> {
        let path = self.meta_path(item);
        let file_meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err("the trash record is missing".into());
            }
            Err(e) => return Err(format!("the trash record cannot be read: {e}")),
        };
        if !file_meta.is_file() || file_meta.len() > META_MAX_BYTES {
            return Err("the trash record is damaged".into());
        }
        let bytes = fs::read(&path).map_err(|e| format!("the trash record cannot be read: {e}"))?;
        let meta: Meta = serde_json::from_slice(&bytes)
            .map_err(|e| format!("the trash record is damaged: {e}"))?;
        if meta.format > META_FORMAT {
            return Err(format!(
                "the trash record has format {}, newer than {META_FORMAT}",
                meta.format
            ));
        }
        Ok(meta)
    }

    fn write_meta(&self, item: &str, meta: &Meta) -> vl_core::Result<()> {
        let bytes = serde_json::to_vec_pretty(meta).map_err(|e| Error::internal(e.to_string()))?;
        write_atomic(&self.meta_path(item), &bytes)?;
        Ok(())
    }

    /// Moves the folder `src` to `dst`: one rename on the same volume; across volumes park,
    /// copy, check, commit and erase (see the module documentation). Returns the links left out
    /// by a copy.
    fn move_dir(
        &self,
        src: &Path,
        dst: &Path,
        park: &Path,
        staging: &Path,
    ) -> vl_core::Result<Vec<PathBuf>> {
        if !self.always_copy {
            match fs::rename(src, dst) {
                Ok(()) => return Ok(Vec::new()),
                Err(e) if crosses_devices(&e) => {}
                Err(e) => return Err(io_err(dst, e)),
            }
        }
        let kind = fs::symlink_metadata(src).map_err(|e| io_err(src, e))?;
        if kind.file_type().is_symlink() {
            return Err(Error::unsupported(format!(
                "{} is a link and cannot be moved to another drive; delete it permanently",
                src.display()
            )));
        }
        fs::rename(src, park).map_err(|e| io_err(src, e))?;
        let copied = (|| {
            remove_tree(staging).map_err(|e| io_err(staging, e))?;
            let mut links = Vec::new();
            copy_tree(park, staging, &mut links)?;
            if manifest(park)? != manifest(staging)? {
                return Err(Error::new(
                    ErrorKind::Integrity,
                    format!(
                        "the copy in {} does not match {}",
                        staging.display(),
                        park.display()
                    ),
                ));
            }
            sync_dir(staging);
            fs::rename(staging, dst).map_err(|e| io_err(dst, e))?;
            if let Some(parent) = dst.parent() {
                sync_dir(parent);
            }
            Ok(links)
        })();
        match copied {
            Ok(links) => {
                // The commit is done; a source that cannot be erased now is erased by
                // `recover` and `collect_garbage` at the next start.
                let _ = remove_tree(park);
                Ok(links)
            }
            Err(e) => {
                let _ = remove_tree(staging);
                let _ = fs::rename(park, src);
                Err(e)
            }
        }
    }
}

#[cfg(test)]
impl Trash {
    /// The same trash, moving by copy as if it were on another volume.
    pub(crate) fn always_copying(mut self) -> Self {
        self.always_copy = true;
        self
    }
}

fn item_of(item: &str, meta: &Meta) -> TrashItem {
    let instance = if naming::is_plain_name(&meta.instance) {
        meta.instance.clone()
    } else {
        item.to_owned()
    };
    TrashItem {
        id: item.to_owned(),
        instance,
        name: meta.name.clone(),
        group: meta.group.clone(),
        game_version: meta.game_version.clone(),
        loader: meta.loader.clone(),
        deleted_unix: meta.deleted_unix,
        size: meta.size,
        comment: meta.comment.clone(),
        problem: None,
    }
}

/// `<id>-<unix time>` or `<id>-<unix time>-<n>`, as earlier versions named deleted instances.
fn split_legacy_name(name: &str) -> (String, Option<i64>) {
    fn digits(s: &str) -> bool {
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
    }
    fn split(s: &str) -> Option<(&str, i64)> {
        let (id, time) = s.rsplit_once('-')?;
        if id.is_empty() || !digits(time) || time.len() < 9 {
            return None;
        }
        Some((id, time.parse().ok()?))
    }
    let found = split(name).or_else(|| {
        let (head, n) = name.rsplit_once('-')?;
        if digits(n) { split(head) } else { None }
    });
    match found {
        Some((id, time)) => (id.to_owned(), Some(time)),
        None => (name.to_owned(), None),
    }
}

/// Entries of `dir` named `<prefix><rest>`, with `rest`.
fn hidden_entries(dir: &Path, prefix: &str) -> Vec<(PathBuf, String)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(PathBuf, String)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let rest = name.strip_prefix(prefix)?.to_owned();
            Some((e.path(), rest))
        })
        .collect();
    found.sort();
    found
}

/// A free `<dir>/<prefix><name>[-n]`.
fn free_hidden(dir: &Path, prefix: &str, name: &str) -> PathBuf {
    let mut n = 1u32;
    loop {
        let candidate = if n == 1 {
            dir.join(format!("{prefix}{name}"))
        } else {
            dir.join(format!("{prefix}{name}-{n}"))
        };
        if fs::symlink_metadata(&candidate).is_err() {
            return candidate;
        }
        n += 1;
    }
}

/// A free `<dir>/<name>[-n]`.
fn free_plain(dir: &Path, name: &str) -> PathBuf {
    free_hidden(dir, "", name)
}

/// Removes a file, a link or a folder tree without following links; a missing path is fine.
fn remove_tree(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(path),
        Ok(m) if m.file_type().is_symlink() => {
            // A link to a folder is a directory entry on Windows.
            fs::remove_file(path).or_else(|_| fs::remove_dir(path))
        }
        Ok(_) => fs::remove_file(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Whether a rename failed because the two paths are on different volumes.
fn crosses_devices(e: &io::Error) -> bool {
    // ERROR_NOT_SAME_DEVICE on Windows, EXDEV elsewhere.
    const RAW: i32 = if cfg!(windows) { 17 } else { 18 };
    e.kind() == io::ErrorKind::CrossesDevices || e.raw_os_error() == Some(RAW)
}

/// Copies the folder `src` to the new folder `dst`, each file flushed to disk; links are left
/// out and listed in `links`.
fn copy_tree(src: &Path, dst: &Path, links: &mut Vec<PathBuf>) -> vl_core::Result<()> {
    fs::create_dir(dst).map_err(|e| io_err(dst, e))?;
    let mut names: Vec<_> = fs::read_dir(src)
        .map_err(|e| io_err(src, e))?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<io::Result<_>>()
        .map_err(|e| io_err(src, e))?;
    names.sort();
    for name in names {
        let from = src.join(&name);
        let to = dst.join(&name);
        let meta = fs::symlink_metadata(&from).map_err(|e| io_err(&from, e))?;
        if meta.is_dir() {
            copy_tree(&from, &to, links)?;
        } else if meta.is_file() {
            copy_file(&from, &to, &meta)?;
        } else {
            links.push(from);
        }
    }
    Ok(())
}

fn copy_file(from: &Path, to: &Path, meta: &fs::Metadata) -> vl_core::Result<()> {
    let mut input = File::open(from).map_err(|e| io_err(from, e))?;
    let mut output = File::create_new(to).map_err(|e| io_err(to, e))?;
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = input.read(&mut buffer).map_err(|e| io_err(from, e))?;
        if n == 0 {
            break;
        }
        output
            .write_all(buffer.get(..n).unwrap_or_default())
            .map_err(|e| io_err(to, e))?;
    }
    if let Ok(modified) = meta.modified() {
        let _ = output.set_modified(modified);
    }
    output.sync_all().map_err(|e| io_err(to, e))?;
    drop(output);
    fs::set_permissions(to, meta.permissions()).map_err(|e| io_err(to, e))?;
    Ok(())
}

/// The folders and files below `root` with the size of each file; links are left out.
fn manifest(root: &Path) -> vl_core::Result<BTreeMap<PathBuf, Option<u64>>> {
    fn walk(
        root: &Path,
        dir: &Path,
        out: &mut BTreeMap<PathBuf, Option<u64>>,
    ) -> vl_core::Result<()> {
        for entry in fs::read_dir(dir).map_err(|e| io_err(dir, e))? {
            let path = entry.map_err(|e| io_err(dir, e))?.path();
            let meta = fs::symlink_metadata(&path).map_err(|e| io_err(&path, e))?;
            let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            if meta.is_dir() {
                out.insert(relative, None);
                walk(root, &path, out)?;
            } else if meta.is_file() {
                out.insert(relative, Some(meta.len()));
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out)?;
    Ok(out)
}

/// Flushes a folder's entries on systems that allow it (a no-op on Windows).
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

#[cfg(test)]
#[path = "trash_tests.rs"]
mod tests;
