//! Updating an instance to another version of its modpack without losing the user's changes.
//!
//! The pack's own state is the baseline: `pack.json` holds the files and overrides the installed
//! pack delivered, with hashes. [`prepare`] fetches the new pack, compares the old baseline, the
//! new index and the folder as it is now, and decides for every file:
//!
//! - the pack added or changed a file the folder still has as delivered: it is added or replaced
//!   (a disabled file is replaced in place and stays disabled; when the pack swaps a mod for
//!   another version of the same project, the user's disabled state moves to the new file);
//! - the user deleted the file, replaced it with another version of the project, or changed it:
//!   it is left alone and not brought back;
//! - the pack dropped a file the user did not touch: it is removed;
//! - a config (an override) the pack changed but the user also changed is not overwritten: the
//!   pack's new version is saved beside it as `<file>.pack-new`;
//! - files the pack never had are never touched.
//!
//! With `reset` the folder is made to match the pack instead.
//!
//! A pack installed from CurseForge is updated by the same rules ([`NewPack::CurseForge`]): its
//! new files are resolved through the API by [`crate::cfpack_update`], files the pack does not
//! require are added disabled, and a file that cannot be downloaded is left to the user while
//! the old file of its project stays.
//!
//! [`apply`] downloads everything first, copies each file it is about to change or remove into
//! `<instance>/.vibelauncher/backup/<time>/`, then makes the changes one by one; a failure or a
//! cancel puts every file (and `pack.json` and `resources.json`) back as it was. The loader and
//! game version of the instance are changed by the caller through the `after` step, which is
//! inside the same all-or-nothing scope.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use vl_core::catalog::{ReleaseChannel, VersionFilter};
use vl_core::install::{
    MinStability, PackAction, PackChange, PackChanges, PackUpdateInfo, PackVersionChange,
    VersionLabel,
};
use vl_core::resources::ResourceKind;
use vl_core::{Error, ErrorKind, Result};
use vl_modrinth::mrpack::{PackFile, PackLoader};
use vl_net::{Client, Control};
use vl_platforms::{CatalogSource, CurseForgeSource};

use crate::cfpack::{self, BlockedFile, CfPack, is_curseforge_pack};
use crate::cfpack_update::CfNew;
use crate::hashing::hash_file;
use crate::mrpack::{
    InstallInput, OverrideRecord, Pack, PackFileRecord, PackKind, PackRecord, PackSourceRecord,
    RECORD_FORMAT, Report, check_cancel, download_files_allowing, fetch, io,
};
use crate::mrpack_origins::{Resolved, kind_and_name, resolve};
use crate::origin::{Origin, OriginStore};

/// Suffix of a file that is switched off.
const DISABLED: &str = ".disabled";
/// Suffix of the pack's new version of a config the user changed.
pub const NEW_SUFFIX: &str = ".pack-new";
/// Folder of the backups inside `<instance>/.vibelauncher/`.
pub const BACKUP_DIR: &str = "backup";
/// Backups kept; older ones are deleted after a successful update.
const KEEP_BACKUPS: usize = 5;
/// Scratch folder for the downloaded pack and files.
const SCRATCH_DIR: &str = "pack-update";

/// What an update works on.
pub struct Context<'a> {
    /// The catalog the pack came from (also used to find the origin of the new files).
    pub source: &'a dyn CatalogSource,
    /// CurseForge, for packs installed from it: their files are resolved by id.
    pub curseforge: Option<&'a CurseForgeSource>,
    /// Downloads.
    pub client: &'a Client,
    /// The instance's game directory.
    pub game_dir: &'a Path,
    /// The instance's `.vibelauncher` directory.
    pub meta_dir: &'a Path,
    /// Cancellation and progress.
    pub control: &'a Control,
}

/// Which new version to take.
#[derive(Debug, Clone, Copy, Default)]
pub struct Wanted<'a> {
    /// The least stable channel when the newest version is chosen.
    pub min: MinStability,
    /// A version id of the pack project; may be older than the installed one.
    pub version: Option<&'a str>,
    /// A local pack file (`.mrpack`, or a CurseForge zip) for a pack that has no catalog source.
    pub file: Option<&'a Path>,
    /// Put every file back as the pack has it.
    pub reset: bool,
}

fn label(v: &vl_core::catalog::ProjectVersion) -> VersionLabel {
    VersionLabel {
        id: v.id.clone(),
        number: v.version_number.clone(),
        channel: v.channel,
    }
}

/// Whether a version carries a pack file of this format.
fn has_pack_file(v: &vl_core::catalog::ProjectVersion, kind: PackKind) -> bool {
    let extension = match kind {
        PackKind::Modrinth => ".mrpack",
        PackKind::CurseForge => ".zip",
    };
    v.files
        .iter()
        .any(|f| f.filename.to_ascii_lowercase().ends_with(extension))
}

/// Looks for a version of the pack's project newer than the installed one, in a channel `min`
/// allows.
///
/// # Errors
///
/// The errors of the catalog.
pub fn check(ctx: &Context<'_>, record: &PackRecord, min: MinStability) -> Result<PackUpdateInfo> {
    let Some(src) = &record.source else {
        return Ok(PackUpdateInfo {
            name: record.name.clone(),
            current: VersionLabel {
                id: String::new(),
                number: record.pack_version.clone(),
                channel: ReleaseChannel::Unknown,
            },
            latest: None,
            from_file: true,
            warnings: Vec::new(),
        });
    };
    let versions = ctx
        .source
        .versions(&src.project_id, &VersionFilter::default(), ctx.control)?;
    let mut warnings = Vec::new();
    let position = versions.iter().position(|v| v.id == src.version_id);
    let current = position.map_or_else(
        || {
            warnings.push(format!(
                "the installed version {} is no longer listed",
                src.version_number
            ));
            VersionLabel {
                id: src.version_id.clone(),
                number: src.version_number.clone(),
                channel: ReleaseChannel::Unknown,
            }
        },
        |i| label(&versions[i]),
    );
    // The list is newest first: a newer version sits before the installed one.
    let newer = &versions[..position.unwrap_or(versions.len())];
    let latest = newer
        .iter()
        .find(|v| {
            min.rank(v.channel).is_some() && has_pack_file(v, record.kind) && v.id != src.version_id
        })
        .map(label);
    Ok(PackUpdateInfo {
        name: record.name.clone(),
        current,
        latest,
        from_file: false,
        warnings,
    })
}

/// Removes the scratch folder when dropped.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug, Clone)]
enum Source {
    Download(PackFile),
    Override(String),
}

#[derive(Debug, Clone)]
enum OpKind {
    Put(Source),
    Delete,
    Move { from: String },
}

/// One step on a path in the game directory.
#[derive(Debug, Clone)]
struct Op {
    target: String,
    kind: OpKind,
}

/// The new version of a pack, in its format.
#[derive(Debug, Clone)]
pub enum NewPack {
    /// A Modrinth `.mrpack`.
    Modrinth(Pack),
    /// A CurseForge zip with its files resolved through the API.
    CurseForge(Box<CfNew>),
}

impl NewPack {
    fn name(&self) -> &str {
        match self {
            Self::Modrinth(p) => &p.index.name,
            Self::CurseForge(c) => &c.pack.manifest.name,
        }
    }

    /// The pack's own version label.
    fn label(&self) -> &str {
        match self {
            Self::Modrinth(p) => &p.index.version_id,
            Self::CurseForge(c) => &c.pack.manifest.version,
        }
    }

    fn game_version(&self) -> Option<&str> {
        match self {
            Self::Modrinth(p) => p.index.game_version(),
            Self::CurseForge(c) => Some(c.pack.game_version()).filter(|v| !v.is_empty()),
        }
    }

    /// The pack's mod loader and its version.
    ///
    /// # Errors
    ///
    /// `Unsupported` for a CurseForge loader the launcher cannot install.
    pub fn loader(&self) -> Result<Option<(PackLoader, String)>> {
        match self {
            Self::Modrinth(p) => Ok(p.index.loader().map(|(l, v)| (l, v.to_owned()))),
            Self::CurseForge(c) => c.pack.loader(),
        }
    }

    /// The files the client gets.
    fn files(&self) -> Vec<&PackFile> {
        match self {
            Self::Modrinth(p) => p.client_files().collect(),
            Self::CurseForge(c) => c.files().collect(),
        }
    }

    fn override_hashes(&self) -> Result<Vec<OverrideRecord>> {
        match self {
            Self::Modrinth(p) => p.override_hashes(),
            Self::CurseForge(c) => c.pack.override_hashes(),
        }
    }

    fn extract_overrides_to(&self, wanted: &[(String, PathBuf)], control: &Control) -> Result<()> {
        match self {
            Self::Modrinth(p) => p.extract_overrides_to(wanted, control),
            Self::CurseForge(c) => c.pack.extract_overrides_to(wanted, control),
        }
    }
}

/// An update worked out and ready to apply.
pub struct Prepared {
    /// What it would do.
    pub changes: PackChanges,
    /// The new pack.
    pub pack: NewPack,
    source: Option<PackSourceRecord>,
    ops: Vec<Op>,
    new_overrides: Vec<OverrideRecord>,
    resolved: Resolved,
    known: HashSet<String>,
    removed: Vec<(ResourceKind, String)>,
    scratch: Scratch,
    /// The baseline files of the installed pack.
    baseline: Vec<PackFileRecord>,
    /// Paths of new files that may fail to download and are then left to the user.
    may_fail: HashSet<String>,
    /// Project id of each new file, by path.
    new_projects: HashMap<String, String>,
    /// Project id of each baseline file, by path, when its origin is known.
    old_projects: HashMap<String, String>,
    /// The new loader, as the pack names it: key and version.
    pub new_loader: Option<(String, String)>,
    /// The new loader and its version.
    pub loader: Option<(PackLoader, String)>,
    /// The new Minecraft version.
    pub new_game_version: String,
}

/// The state of a path in the game directory.
#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Enabled(String),
    Disabled(String),
    Missing,
}

impl State {
    fn hash(&self) -> Option<&str> {
        match self {
            Self::Enabled(h) | Self::Disabled(h) => Some(h),
            Self::Missing => None,
        }
    }
}

fn state(game: &Path, rel: &str) -> State {
    let sha = |p: &Path| hash_file(p).map_or_else(|_| "unreadable".to_owned(), |h| h.sha1);
    let on = game.join(rel);
    if fs::symlink_metadata(&on).is_ok() {
        return State::Enabled(sha(&on));
    }
    let off = game.join(format!("{rel}{DISABLED}"));
    if fs::symlink_metadata(&off).is_ok() {
        return State::Disabled(sha(&off));
    }
    State::Missing
}

/// The name of a mod file without its version: `sodium-fabric-0.5.0+mc1.21.jar` and
/// `sodium-fabric-0.6.1+mc1.21.jar` both give `sodium-fabric`.
fn stem(path: &str) -> Option<String> {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    let name = name
        .strip_suffix(".jar")
        .or_else(|| name.strip_suffix(".zip"))
        .unwrap_or(&name);
    let b = name.as_bytes();
    let sep = |c: u8| matches!(c, b'-' | b'_' | b'+' | b' ');
    let cut = (1..b.len())
        .find(|&i| {
            sep(b[i - 1])
                && (b[i].is_ascii_digit()
                    || (b[i] == b'v' && b.get(i + 1).is_some_and(u8::is_ascii_digit)))
        })
        .unwrap_or(b.len());
    let stem = name[..cut].trim_end_matches(['-', '_', '+', ' ']);
    (!stem.is_empty()).then(|| stem.to_owned())
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn change(path: &str, action: PackAction, download: bool) -> PackChange {
    PackChange {
        path: path.to_owned(),
        action,
        download,
        disabled: false,
        note: None,
    }
}

/// What the user did with the slot of a successor of a removed file.
enum Succ {
    Disabled,
    Deleted(String),
    Modified(String),
}

struct Planner<'a> {
    game: &'a Path,
    reset: bool,
    old_project: HashMap<String, String>,
    new_project: HashMap<String, String>,
    origins: &'a [Origin],
    changes: Vec<PackChange>,
    ops: Vec<Op>,
    unchanged: u32,
    bytes: u64,
    /// New paths that are added disabled (CurseForge files the pack does not require).
    off: HashSet<String>,
    /// Projects whose new file the user has to download by hand: their old file stays.
    keep_projects: HashSet<String>,
}

impl Planner<'_> {
    /// Where a new file goes when it is added: disabled when the pack does not require it.
    fn add_target(&self, path: &str) -> (String, bool) {
        if self.off.contains(path) {
            (format!("{path}{DISABLED}"), true)
        } else {
            (path.to_owned(), false)
        }
    }

    fn put_download(&mut self, target: String, file: &PackFile) {
        self.bytes += file.size;
        self.ops.push(Op {
            target,
            kind: OpKind::Put(Source::Download(file.clone())),
        });
    }

    fn delete(&mut self, target: String) {
        self.ops.push(Op {
            target,
            kind: OpKind::Delete,
        });
    }

    fn put_override(&mut self, target: String, rel: &str) {
        self.ops.push(Op {
            target,
            kind: OpKind::Put(Source::Override(rel.to_owned())),
        });
    }

    /// Another file of the same project is in the folder where the pack's old `file` was.
    fn replacement_of(&self, file: &PackFileRecord) -> Option<String> {
        let (kind, name) = kind_and_name(&file.path)?;
        let project = self.old_project.get(&file.sha1)?;
        self.origins
            .iter()
            .filter(|o| o.kind == kind && o.file != name && &o.project_id == project)
            .find(|o| {
                let base = format!("{}/{}", kind.dir(), o.file);
                state(self.game, &base) != State::Missing
            })
            .map(|o| o.file.clone())
    }

    fn successor<'p>(
        &self,
        old: &PackFileRecord,
        pool: &[&'p PackFile],
        taken: &HashSet<String>,
    ) -> Option<&'p PackFile> {
        let project = self.old_project.get(&old.sha1);
        let old_stem = stem(&old.path);
        pool.iter()
            .copied()
            .filter(|a| !taken.contains(&a.path) && parent(&a.path) == parent(&old.path))
            .find(|a| {
                let by_project = project.is_some_and(|p| self.new_project.get(&a.path) == Some(p));
                by_project || (old_stem.is_some() && stem(&a.path) == old_stem)
            })
    }

    fn files(&mut self, record: &PackRecord, new: &[&PackFile]) {
        let old: HashMap<&str, &PackFileRecord> =
            record.files.iter().map(|f| (f.path.as_str(), f)).collect();
        let newmap: HashMap<&str, &PackFile> = new.iter().map(|f| (f.path.as_str(), *f)).collect();
        if self.reset {
            return self.files_reset(record, new, &newmap);
        }
        let added: Vec<&PackFile> = new
            .iter()
            .copied()
            .filter(|f| !old.contains_key(f.path.as_str()))
            .collect();
        let mut succ: HashMap<String, Succ> = HashMap::new();
        for r in record
            .files
            .iter()
            .filter(|r| !newmap.contains_key(r.path.as_str()))
        {
            let st = state(self.game, &r.path);
            let blocked = self
                .old_project
                .get(&r.sha1)
                .is_some_and(|p| self.keep_projects.contains(p));
            if blocked && st != State::Missing {
                let mut c = change(&r.path, PackAction::KeepModified, true);
                c.note = Some(
                    "the pack's new version of it has to be downloaded by hand; this one stays \
                     until you replace it"
                        .into(),
                );
                self.changes.push(c);
                continue;
            }
            let taken: HashSet<String> = succ.keys().cloned().collect();
            let next = self.successor(r, &added, &taken).map(|a| a.path.clone());
            match &st {
                State::Missing => {
                    let note = match self.replacement_of(r) {
                        Some(by) => format!("you replaced {} with {by}", r.path),
                        None => format!("you removed {}", r.path),
                    };
                    if let Some(n) = next {
                        succ.insert(n, Succ::Deleted(note));
                    }
                }
                State::Enabled(h) | State::Disabled(h) if *h == r.sha1 => {
                    let disabled = matches!(st, State::Disabled(_));
                    let target = if disabled {
                        format!("{}{DISABLED}", r.path)
                    } else {
                        r.path.clone()
                    };
                    self.delete(target);
                    let mut c = change(&r.path, PackAction::Remove, true);
                    c.disabled = disabled;
                    self.changes.push(c);
                    if disabled && let Some(n) = next {
                        succ.insert(n, Succ::Disabled);
                    }
                }
                _ => {
                    let mut c = change(&r.path, PackAction::KeepModified, true);
                    c.note = Some("changed by you; the new pack no longer has it".into());
                    self.changes.push(c);
                    if let Some(n) = next {
                        succ.insert(n, Succ::Modified(format!("you changed {}", r.path)));
                    }
                }
            }
        }
        for f in new {
            let Some(o) = old.get(f.path.as_str()) else {
                self.added(f, succ.remove(&f.path));
                continue;
            };
            if o.sha1.eq_ignore_ascii_case(&f.sha1) {
                self.unchanged += 1;
                continue;
            }
            match state(self.game, &f.path) {
                State::Missing => {
                    let mut c = change(&f.path, PackAction::KeepDeleted, true);
                    c.note = Some("you removed it; it stays removed".into());
                    self.changes.push(c);
                }
                st @ (State::Enabled(_) | State::Disabled(_)) => {
                    let h = st.hash().unwrap_or_default().to_owned();
                    let disabled = matches!(st, State::Disabled(_));
                    if h == f.sha1.to_ascii_lowercase() {
                        self.unchanged += 1;
                    } else if h == o.sha1.to_ascii_lowercase() {
                        let target = if disabled {
                            format!("{}{DISABLED}", f.path)
                        } else {
                            f.path.clone()
                        };
                        self.put_download(target, f);
                        let mut c = change(&f.path, PackAction::Update, true);
                        c.disabled = disabled;
                        self.changes.push(c);
                    } else {
                        let mut c = change(&f.path, PackAction::KeepModified, true);
                        c.note =
                            Some("changed by you; the pack's new version is not applied".into());
                        self.changes.push(c);
                    }
                }
            }
        }
    }

    fn added(&mut self, f: &PackFile, succ: Option<Succ>) {
        match succ {
            Some(Succ::Deleted(note)) => {
                let mut c = change(&f.path, PackAction::KeepDeleted, true);
                c.note = Some(format!("{note}; it stays removed"));
                self.changes.push(c);
            }
            Some(Succ::Modified(note)) => {
                let mut c = change(&f.path, PackAction::KeepModified, true);
                c.note = Some(format!("{note}; the new file is not added"));
                self.changes.push(c);
            }
            Some(Succ::Disabled) => {
                self.put_download(format!("{}{DISABLED}", f.path), f);
                let mut c = change(&f.path, PackAction::Add, true);
                c.disabled = true;
                c.note = Some("the file it replaces was disabled; this one is too".into());
                self.changes.push(c);
            }
            None => match state(self.game, &f.path) {
                State::Missing => {
                    let (target, off) = self.add_target(&f.path);
                    self.put_download(target, f);
                    let mut c = change(&f.path, PackAction::Add, true);
                    c.disabled = off;
                    self.changes.push(c);
                }
                st if st.hash() == Some(&f.sha1.to_ascii_lowercase()) => self.unchanged += 1,
                _ => {
                    let mut c = change(&f.path, PackAction::KeepModified, true);
                    c.note = Some("a different file is already there".into());
                    self.changes.push(c);
                }
            },
        }
    }

    fn files_reset(
        &mut self,
        record: &PackRecord,
        new: &[&PackFile],
        newmap: &HashMap<&str, &PackFile>,
    ) {
        for r in record
            .files
            .iter()
            .filter(|r| !newmap.contains_key(r.path.as_str()))
        {
            match state(self.game, &r.path) {
                State::Missing => {}
                st => {
                    let disabled = matches!(st, State::Disabled(_));
                    self.delete(if disabled {
                        format!("{}{DISABLED}", r.path)
                    } else {
                        r.path.clone()
                    });
                    let mut c = change(&r.path, PackAction::Remove, true);
                    c.disabled = disabled;
                    self.changes.push(c);
                }
            }
        }
        for f in new {
            let want = f.sha1.to_ascii_lowercase();
            match state(self.game, &f.path) {
                State::Enabled(h) if h == want => self.unchanged += 1,
                State::Disabled(h) if h == want => {
                    self.ops.push(Op {
                        target: f.path.clone(),
                        kind: OpKind::Move {
                            from: format!("{}{DISABLED}", f.path),
                        },
                    });
                    self.changes.push(change(&f.path, PackAction::Enable, true));
                }
                State::Missing => {
                    let (target, off) = self.add_target(&f.path);
                    self.put_download(target, f);
                    let mut c = change(&f.path, PackAction::Add, true);
                    c.disabled = off;
                    self.changes.push(c);
                }
                st => {
                    if matches!(st, State::Disabled(_)) {
                        self.delete(format!("{}{DISABLED}", f.path));
                    }
                    self.put_download(f.path.clone(), f);
                    self.changes.push(change(&f.path, PackAction::Update, true));
                }
            }
        }
    }

    fn overrides(&mut self, record: &PackRecord, new: &[OverrideRecord]) {
        let old: HashMap<&str, &str> = record
            .overrides
            .iter()
            .map(|o| (o.path.as_str(), o.sha1.as_str()))
            .collect();
        let newmap: HashSet<&str> = new.iter().map(|o| o.path.as_str()).collect();
        for n in new {
            let prior = old.get(n.path.as_str()).copied();
            let st = state(self.game, &n.path);
            let disabled = matches!(st, State::Disabled(_));
            let at = |rel: &str| {
                if disabled {
                    format!("{rel}{DISABLED}")
                } else {
                    rel.to_owned()
                }
            };
            if self.reset {
                match &st {
                    State::Enabled(h) if *h == n.sha1 => self.unchanged += 1,
                    State::Disabled(h) if *h == n.sha1 => {
                        self.ops.push(Op {
                            target: n.path.clone(),
                            kind: OpKind::Move { from: at(&n.path) },
                        });
                        self.changes
                            .push(change(&n.path, PackAction::Enable, false));
                    }
                    State::Missing => {
                        self.put_override(n.path.clone(), &n.path);
                        self.changes.push(change(&n.path, PackAction::Add, false));
                    }
                    _ => {
                        if disabled {
                            self.delete(at(&n.path));
                        }
                        self.put_override(n.path.clone(), &n.path);
                        self.changes
                            .push(change(&n.path, PackAction::Update, false));
                    }
                }
                continue;
            }
            if prior == Some(n.sha1.as_str()) {
                self.unchanged += 1;
                continue;
            }
            match st.hash() {
                None if prior.is_some() => {
                    let mut c = change(&n.path, PackAction::KeepDeleted, false);
                    c.note = Some("you removed it; it stays removed".into());
                    self.changes.push(c);
                }
                None => {
                    self.put_override(n.path.clone(), &n.path);
                    self.changes.push(change(&n.path, PackAction::Add, false));
                }
                Some(h) if h == n.sha1 => self.unchanged += 1,
                Some(h) if Some(h) == prior => {
                    self.put_override(at(&n.path), &n.path);
                    let mut c = change(&n.path, PackAction::Update, false);
                    c.disabled = disabled;
                    self.changes.push(c);
                }
                Some(_) => {
                    self.put_override(format!("{}{NEW_SUFFIX}", n.path), &n.path);
                    let mut c = change(&n.path, PackAction::SaveNew, false);
                    c.note = Some(format!(
                        "changed by you; the pack's new version is saved as {}{NEW_SUFFIX}",
                        n.path
                    ));
                    self.changes.push(c);
                }
            }
        }
        for o in record
            .overrides
            .iter()
            .filter(|o| !newmap.contains(o.path.as_str()))
        {
            match state(self.game, &o.path) {
                State::Missing => {}
                st if st.hash() == Some(o.sha1.as_str()) => {
                    let disabled = matches!(st, State::Disabled(_));
                    self.delete(if disabled {
                        format!("{}{DISABLED}", o.path)
                    } else {
                        o.path.clone()
                    });
                    self.changes
                        .push(change(&o.path, PackAction::Remove, false));
                }
                _ => {
                    let mut c = change(&o.path, PackAction::KeepModified, false);
                    c.note = Some("changed by you; the new pack no longer has it".into());
                    self.changes.push(c);
                }
            }
        }
    }
}

fn loader_text(l: Option<&(String, String)>) -> String {
    l.map_or_else(|| "none".to_owned(), |(k, v)| format!("{k} {v}"))
}

/// CurseForge, for a pack installed from it.
fn curseforge<'a>(ctx: &Context<'a>) -> Result<&'a CurseForgeSource> {
    ctx.curseforge
        .ok_or_else(|| Error::unsupported("CurseForge is not available here"))
}

/// Fetches or opens the new pack in the format of the installed one. `None` when there is
/// nothing newer and nothing else was asked.
fn open_new(
    ctx: &Context<'_>,
    record: &PackRecord,
    wanted: &Wanted<'_>,
    dir: &Path,
) -> Result<Option<(NewPack, Option<PackSourceRecord>)>> {
    let from_curseforge = record.kind == PackKind::CurseForge;
    let (file, source) = match (&record.source, wanted.file) {
        (Some(src), None) => {
            let version = match wanted.version {
                Some(v) => v.to_owned(),
                None => match check(ctx, record, wanted.min)?.latest {
                    Some(l) => l.id,
                    None if wanted.reset => src.version_id.clone(),
                    None => return Ok(None),
                },
            };
            let (file, source) = if from_curseforge {
                cfpack::fetch(
                    curseforge(ctx)?,
                    ctx.client,
                    &src.project_id,
                    Some(&version),
                    wanted.min,
                    dir,
                    ctx.control,
                )?
            } else {
                fetch(
                    ctx.source,
                    ctx.client,
                    &src.project_id,
                    Some(&version),
                    wanted.min,
                    dir,
                    ctx.control,
                )?
            };
            (file, Some(source))
        }
        (Some(_), Some(_)) => {
            return Err(Error::invalid_input(
                "this pack comes from the catalog; give a version, not a file",
            ));
        }
        (None, Some(file)) => (file.to_path_buf(), None),
        (None, None) => {
            return Err(Error::invalid_input(
                "this pack was installed from a file; give the new pack file",
            ));
        }
    };
    if !from_curseforge {
        return Ok(Some((NewPack::Modrinth(Pack::open(&file)?), source)));
    }
    if !is_curseforge_pack(&file)? {
        return Err(Error::invalid_input(
            "the instance was installed from a CurseForge modpack; give a CurseForge pack zip",
        ));
    }
    let pack = CfPack::open(&file)?;
    let resolved = cfpack::resolve(curseforge(ctx)?, &pack, ctx.control)?;
    let new = CfNew::new(pack, resolved, record);
    Ok(Some((NewPack::CurseForge(Box::new(new)), source)))
}

/// Fetches the new pack and works out what updating to it does.
///
/// Returns `None` when there is nothing newer, no version was asked for and no reset was asked.
///
/// # Errors
///
/// `InvalidInput` when a pack from a file is given no file (or a catalog pack is given one),
/// `NotFound`, `Network`, `Integrity`, `Unsupported`, `Cancelled`.
pub fn prepare(
    ctx: &Context<'_>,
    record: &PackRecord,
    wanted: &Wanted<'_>,
) -> Result<Option<Prepared>> {
    let scratch = Scratch(ctx.meta_dir.join(SCRATCH_DIR));
    let _ = fs::remove_dir_all(&scratch.0);
    let Some((pack, source)) = open_new(ctx, record, wanted, &scratch.0.join("pack"))? else {
        return Ok(None);
    };
    check_cancel(ctx.control)?;
    let game_version = pack
        .game_version()
        .ok_or_else(|| Error::invalid_input("the pack does not name a Minecraft version"))?
        .to_owned();
    let loader = pack.loader()?;
    let new_loader = loader
        .as_ref()
        .map(|(l, v)| (l.key().to_owned(), v.clone()));
    let new_overrides = pack.override_hashes()?;
    let new_files: Vec<&PackFile> = pack.files();

    // Files the pack brings that the baseline did not have, or has changed: their projects are
    // looked up so a swapped mod can be matched with the file it replaces.
    let old_hash: HashMap<&str, &str> = record
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.sha1.as_str()))
        .collect();
    let incoming: Vec<String> = new_files
        .iter()
        .filter(|f| old_hash.get(f.path.as_str()) != Some(&f.sha1.to_ascii_lowercase().as_str()))
        .map(|f| f.path.clone())
        .collect();
    let resolved = match &pack {
        NewPack::Modrinth(p) => resolve(
            ctx.source,
            p,
            &Report {
                installed: incoming.clone(),
                ..Report::default()
            },
            ctx.control,
        )?,
        // CurseForge names the project of every file: nothing to look up.
        NewPack::CurseForge(c) => Resolved {
            origins: c.origins(&incoming.iter().cloned().collect()),
            ..Resolved::default()
        },
    };
    let loaded = OriginStore::new(ctx.meta_dir.join("resources.json")).load();
    let new_project: HashMap<String, String> = match &pack {
        NewPack::CurseForge(c) => c.projects(),
        NewPack::Modrinth(_) => resolved
            .origins
            .iter()
            .map(|o| (format!("{}/{}", o.kind.dir(), o.file), o.project_id.clone()))
            .collect(),
    };
    let old_project: HashMap<String, String> = loaded
        .origins
        .iter()
        .map(|o| (o.sha1.clone(), o.project_id.clone()))
        .collect();
    let mut planner = Planner {
        game: ctx.game_dir,
        reset: wanted.reset,
        old_project,
        new_project,
        origins: &loaded.origins,
        changes: Vec::new(),
        ops: Vec::new(),
        unchanged: 0,
        bytes: 0,
        off: match &pack {
            NewPack::CurseForge(c) => c.optional(),
            NewPack::Modrinth(_) => HashSet::new(),
        },
        keep_projects: match &pack {
            NewPack::CurseForge(c) => c.blocked_projects(),
            NewPack::Modrinth(_) => HashSet::new(),
        },
    };
    planner.files(record, &new_files);
    planner.overrides(record, &new_overrides);
    let old_projects: HashMap<String, String> = record
        .files
        .iter()
        .filter_map(|f| {
            let project = planner.old_project.get(&f.sha1)?;
            Some((f.path.clone(), project.clone()))
        })
        .collect();
    let new_projects = planner.new_project.clone();

    let mut warnings = Vec::new();
    if let NewPack::CurseForge(c) = &pack {
        warnings.extend(c.notes());
    }
    if resolved.lookup_failed {
        warnings.push(
            "could not look up the new files in the catalog; replaced mods are matched by name"
                .to_owned(),
        );
    }
    if record.overrides.is_empty() && !new_overrides.is_empty() {
        warnings.push(
            "this pack was installed before configs were tracked: every config that differs from the new pack is kept and the new one is saved beside it"
                .to_owned(),
        );
    }
    let game_version_change = (record.game_version != game_version).then(|| PackVersionChange {
        from: record.game_version.clone(),
        to: game_version.clone(),
    });
    if game_version_change.is_some() {
        warnings.push(
            "the Minecraft version changes; mods you added yourself may not work with it"
                .to_owned(),
        );
    }
    let loader_change = (record.loader != new_loader).then(|| PackVersionChange {
        from: loader_text(record.loader.as_ref()),
        to: loader_text(new_loader.as_ref()),
    });
    let changes = PackChanges {
        name: pack.name().to_owned(),
        from: record
            .source
            .as_ref()
            .map_or_else(|| record.pack_version.clone(), |s| s.version_number.clone()),
        to: source
            .as_ref()
            .map_or_else(|| pack.label().to_owned(), |s| s.version_number.clone()),
        game_version: game_version_change,
        loader: loader_change,
        changes: planner.changes,
        unchanged: planner.unchanged,
        download_bytes: planner.bytes,
        reset: wanted.reset,
        warnings,
    };
    let mut known: HashSet<String> = record
        .files
        .iter()
        .filter(|f| f.origin_known)
        .filter(|f| {
            new_files
                .iter()
                .any(|n| n.path == f.path && n.sha1.eq_ignore_ascii_case(&f.sha1))
        })
        .map(|f| f.path.clone())
        .collect();
    known.extend(
        resolved
            .origins
            .iter()
            .map(|o| format!("{}/{}", o.kind.dir(), o.file)),
    );
    let removed = changes
        .changes
        .iter()
        .filter(|c| c.action == PackAction::Remove && c.download)
        .filter_map(|c| kind_and_name(&c.path).map(|(k, n)| (k, n.to_owned())))
        .collect();
    let may_fail = match &pack {
        NewPack::CurseForge(c) => c.may_fail(),
        NewPack::Modrinth(_) => HashSet::new(),
    };
    let ops = planner.ops;
    Ok(Some(Prepared {
        changes,
        pack,
        source,
        ops,
        new_overrides,
        resolved,
        known,
        removed,
        scratch,
        baseline: record.files.clone(),
        may_fail,
        new_projects,
        old_projects,
        new_loader,
        loader,
        new_game_version: game_version,
    }))
}

/// What an applied update left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// Where the previous versions of the touched files are.
    pub backup: PathBuf,
    /// Files of a CurseForge pack the user has to download by hand (also kept in `pack.json`).
    pub blocked: Vec<BlockedFile>,
}

/// `YYYYMMDD-HHMMSS` in UTC.
fn timestamp() -> String {
    let secs = crate::origin::now_unix().max(0);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn new_backup_dir(meta: &Path) -> Result<PathBuf> {
    let base = meta.join(BACKUP_DIR);
    fs::create_dir_all(&base).map_err(|e| io(&base, &e))?;
    let stamp = timestamp();
    for n in 0..1000 {
        let name = if n == 0 {
            stamp.clone()
        } else {
            format!("{stamp}-{n}")
        };
        let dir = base.join(name);
        if fs::create_dir(&dir).is_ok() {
            return Ok(dir);
        }
    }
    Err(Error::new(ErrorKind::Io, "could not make a backup folder"))
}

fn prune_backups(meta: &Path) {
    let base = meta.join(BACKUP_DIR);
    let Ok(read) = fs::read_dir(&base) else {
        return;
    };
    let mut dirs: Vec<PathBuf> = read
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    let extra = dirs.len().saturating_sub(KEEP_BACKUPS);
    for dir in dirs.into_iter().take(extra) {
        let _ = fs::remove_dir_all(dir);
    }
}

/// Copies `rel` of the game directory into the backup when it exists.
fn back_up(game: &Path, backup: &Path, rel: &str) -> Result<()> {
    let from = game.join(rel);
    if fs::symlink_metadata(&from).is_err() {
        return Ok(());
    }
    let to = backup.join(rel);
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(|e| io(parent, &e))?;
    }
    fs::copy(&from, &to).map_err(|e| io(&from, &e))?;
    Ok(())
}

/// Puts the touched files and the two records back; returns what could not be restored.
fn roll_back(game: &Path, meta: &Path, backup: &Path, touched: &[String]) -> Vec<String> {
    let mut failed = Vec::new();
    let mut restore = |target: PathBuf, saved: PathBuf| {
        let result = if saved.is_file() {
            saved
                .parent()
                .map_or(Ok(()), fs::create_dir_all)
                .and_then(|()| fs::copy(&saved, &target).map(|_| ()))
        } else {
            match fs::remove_file(&target) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        };
        if let Err(e) = result {
            failed.push(format!("{}: {e}", target.display()));
        }
    };
    for rel in touched.iter().rev() {
        restore(game.join(rel), backup.join(rel));
    }
    for name in [crate::mrpack::PACK_RECORD, "resources.json"] {
        restore(meta.join(name), backup.join(".vibelauncher").join(name));
    }
    failed
}

/// The hashes of the file at `base` (or its disabled copy) in the game directory.
fn placed_hashes(game: &Path, base: &str) -> Option<crate::FileHashes> {
    let on = game.join(base);
    let path = if on.is_file() {
        on
    } else {
        game.join(format!("{base}{DISABLED}"))
    };
    hash_file(&path).ok()
}

/// The record of an updated CurseForge pack: the new pack's files (the user's hand-downloads
/// left out), with the hashes CurseForge gives, else those of the placed file, else the
/// baseline's.
fn curseforge_record(
    new: &CfNew,
    prepared: &Prepared,
    failed: &HashSet<String>,
    placed: &HashSet<String>,
    game: &Path,
    source: Option<PackSourceRecord>,
) -> Result<PackRecord> {
    let old: HashMap<&str, &PackFileRecord> = prepared
        .baseline
        .iter()
        .map(|f| (f.path.as_str(), f))
        .collect();
    let files = new
        .files()
        .filter(|f| !failed.contains(&f.path))
        .map(|f| {
            let fresh = placed
                .contains(&f.path)
                .then(|| placed_hashes(game, &f.path))
                .flatten();
            let before = old
                .get(f.path.as_str())
                .filter(|o| f.sha1.is_empty() || o.sha1.eq_ignore_ascii_case(&f.sha1));
            let (sha1, sha512, size) = match (&fresh, before) {
                (Some(h), _) if f.sha1.is_empty() || h.sha1 == f.sha1 => {
                    (h.sha1.clone(), h.sha512.clone(), h.size)
                }
                (_, Some(o)) => (o.sha1.clone(), o.sha512.clone(), o.size),
                _ => (f.sha1.to_ascii_lowercase(), String::new(), f.size),
            };
            PackFileRecord {
                path: f.path.clone(),
                sha1,
                sha512,
                size,
                optional: f.client == vl_modrinth::mrpack::Support::Optional,
                origin_known: true,
            }
        })
        .collect();
    Ok(PackRecord {
        format: RECORD_FORMAT,
        name: new.pack.manifest.name.clone(),
        pack_version: new.pack.manifest.version.clone(),
        source,
        game_version: prepared.new_game_version.clone(),
        loader: prepared.new_loader.clone(),
        installed_unix: crate::origin::now_unix(),
        files,
        overrides: Vec::new(),
        kind: PackKind::CurseForge,
        blocked: new.blocked(failed),
    })
}

/// Downloads the new files, backs up what is about to change and makes the changes. `after`
/// runs last, inside the same all-or-nothing scope, and changes what is not a file (the game
/// version and loader of the instance).
///
/// On any error or cancel the files and the records are put back as they were.
///
/// # Errors
///
/// `Network`, `Integrity`, `Io`, `Cancelled`, or what `after` returns.
pub fn apply(
    prepared: Prepared,
    ctx: &Context<'_>,
    after: impl FnOnce(&Prepared) -> Result<()>,
) -> Result<Applied> {
    let Prepared {
        ops,
        pack,
        source,
        new_overrides,
        resolved,
        known,
        removed,
        scratch,
        may_fail,
        new_projects,
        old_projects,
        ..
    } = &prepared;
    // Everything is fetched and extracted before the first file is touched.
    let staged = scratch.0.join("staged");
    let mut files = Vec::new();
    let mut file_dests = Vec::new();
    let mut extract = Vec::new();
    let mut staged_of: HashMap<usize, PathBuf> = HashMap::new();
    for (i, op) in ops.iter().enumerate() {
        let OpKind::Put(source) = &op.kind else {
            continue;
        };
        let dest = staged.join(i.to_string()).join("file");
        match source {
            Source::Download(f) => {
                files.push(f);
                file_dests.push(dest.clone());
            }
            Source::Override(rel) => extract.push((rel.clone(), dest.clone())),
        }
        staged_of.insert(i, dest);
    }
    let failed = download_files_allowing(
        &InstallInput {
            client: ctx.client,
            game_dir: ctx.game_dir,
            staging: &staged,
            control: ctx.control,
        },
        &files,
        &file_dests,
        may_fail,
    )?;
    pack.extract_overrides_to(&extract, ctx.control)?;
    check_cancel(ctx.control)?;
    // A file left to the user is not placed, and the old file of its project stays.
    let blocked_projects: HashSet<&String> =
        failed.iter().filter_map(|p| new_projects.get(p)).collect();
    let base = |target: &str| target.strip_suffix(DISABLED).unwrap_or(target).to_owned();
    let skipped: HashSet<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, op)| match &op.kind {
            OpKind::Put(Source::Download(f)) => failed.contains(&f.path),
            OpKind::Delete => old_projects
                .get(&base(&op.target))
                .is_some_and(|p| blocked_projects.contains(p)),
            _ => false,
        })
        .map(|(i, _)| i)
        .collect();
    let kept: HashSet<String> = skipped
        .iter()
        .filter_map(|i| ops.get(*i))
        .filter(|op| matches!(op.kind, OpKind::Delete))
        .map(|op| base(&op.target))
        .collect();
    let removed: Vec<(ResourceKind, String)> = removed
        .iter()
        .filter(|(k, n)| !kept.contains(&format!("{}/{n}", k.dir())))
        .cloned()
        .collect();

    let backup = new_backup_dir(ctx.meta_dir)?;
    let mut touched: Vec<String> = Vec::new();
    let run = (|| -> Result<()> {
        // The records are copied first so a failed write can be undone.
        let meta_backup = backup.join(".vibelauncher");
        fs::create_dir_all(&meta_backup).map_err(|e| io(&meta_backup, &e))?;
        for name in [crate::mrpack::PACK_RECORD, "resources.json"] {
            let from = ctx.meta_dir.join(name);
            if from.is_file() {
                fs::copy(&from, meta_backup.join(name)).map_err(|e| io(&from, &e))?;
            }
        }
        for (i, op) in ops.iter().enumerate() {
            check_cancel(ctx.control)?;
            if skipped.contains(&i) {
                continue;
            }
            let target = ctx.game_dir.join(&op.target);
            touched.push(op.target.clone());
            back_up(ctx.game_dir, &backup, &op.target)?;
            if let OpKind::Move { from } = &op.kind {
                touched.push(from.clone());
                back_up(ctx.game_dir, &backup, from)?;
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|e| io(parent, &e))?;
            }
            match &op.kind {
                OpKind::Put(_) => {
                    let staged = staged_of
                        .get(&i)
                        .ok_or_else(|| Error::internal("a staged file is missing"))?;
                    fs::rename(staged, &target).map_err(|e| io(&target, &e))?;
                }
                OpKind::Delete => match fs::remove_file(&target) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                        return Err(io(&target, &e));
                    }
                    _ => {}
                },
                OpKind::Move { from } => {
                    let from = ctx.game_dir.join(from);
                    fs::rename(&from, &target).map_err(|e| io(&target, &e))?;
                }
            }
        }
        let placed: HashSet<String> = ops
            .iter()
            .enumerate()
            .filter(|(i, o)| {
                !skipped.contains(i) && matches!(&o.kind, OpKind::Put(Source::Download(_)))
            })
            .map(|(_, o)| base(&o.target))
            .collect();
        let game = ctx.game_dir;
        let mut added: Vec<Origin> = resolved
            .origins
            .iter()
            .filter(|o| placed.contains(&format!("{}/{}", o.kind.dir(), o.file)))
            .cloned()
            .collect();
        if matches!(pack, NewPack::CurseForge(_)) {
            // CurseForge may give no hash: the record takes the placed file's.
            added.retain_mut(|o| {
                let base = format!("{}/{}", o.kind.dir(), o.file);
                let Some(hashes) = placed_hashes(game, &base) else {
                    return false;
                };
                o.sha1 = hashes.sha1;
                o.sha512 = Some(hashes.sha512);
                o.size = hashes.size;
                true
            });
        }
        OriginStore::new(ctx.meta_dir.join("resources.json")).record(
            added,
            |o| {
                let base = format!("{}/{}", o.kind.dir(), o.file);
                state(game, &base) != State::Missing
            },
            &removed,
        )?;
        let known: HashSet<&str> = known.iter().map(String::as_str).collect();
        let record = match pack {
            NewPack::Modrinth(p) => {
                let all: Vec<String> = p.client_files().map(|f| f.path.clone()).collect();
                PackRecord::new(
                    p,
                    &Report {
                        installed: all,
                        ..Report::default()
                    },
                    source.clone(),
                )
            }
            NewPack::CurseForge(c) => {
                curseforge_record(c, &prepared, &failed, &placed, game, source.clone())?
            }
        };
        record
            .with_known_origins(&known)
            .with_overrides(new_overrides.clone())
            .save(ctx.meta_dir)?;
        after(&prepared)
    })();
    match run {
        Ok(()) => {
            prune_backups(ctx.meta_dir);
            let blocked = match &prepared.pack {
                NewPack::CurseForge(c) => c.blocked(&failed),
                NewPack::Modrinth(_) => Vec::new(),
            };
            Ok(Applied { backup, blocked })
        }
        Err(e) => {
            let failed = roll_back(ctx.game_dir, ctx.meta_dir, &backup, &touched);
            if failed.is_empty() {
                let _ = fs::remove_dir_all(&backup);
                Err(e)
            } else {
                Err(Error::new(
                    e.kind,
                    format!(
                        "{}; some files could not be restored, copies are in {}: {}",
                        e.message,
                        backup.display(),
                        failed.join("; ")
                    ),
                ))
            }
        }
    }
}
