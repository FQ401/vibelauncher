//! Moving a section to another folder, and removing the old copies afterwards.
//!
//! A copy goes into a temporary folder next to the target (`.<name>.vl-moving`, same drive), is
//! counted again, then its entries are renamed into the target, and only then the path is
//! switched: `storage.json` for a section, the pointer file for the data folder. A failure or a
//! cancel at any step removes what was copied and keeps the old path. The old copy is recorded as
//! a leftover and stays until [`cleanup`]. A switch without copying only changes the path.
//!
//! Links are not followed and not copied; the report lists them.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use vl_core::storage::{CleanupReport, MoveMode, MoveRequest, PathCheck, PathIssue, Section};

use crate::check::check_path;
use crate::layout::{GAME_ENTRIES, Layout, LeftoverRecord, StorageConfig};
use crate::paths::{self, Env, is_within, normalize};
use crate::retention::{self, Policy};
use crate::{StoreError, lock, pointer};

/// Suffix of the temporary folder a copy is made in.
const MOVING_SUFFIX: &str = ".vl-moving";
/// Progress is reported at least after this many bytes.
const REPORT_EVERY: u64 = 4 << 20;
/// Extra space a copy asks for beyond the size of the data.
const SPACE_MARGIN: u64 = 64 << 20;

/// A checked move, ready to run.
#[derive(Debug, Clone)]
pub struct MovePlan {
    /// The section.
    pub section: Section,
    /// Its folder now.
    pub from: PathBuf,
    /// Its new folder.
    pub to: PathBuf,
    /// Copy or switch only.
    pub mode: MoveMode,
    /// Bytes to copy.
    pub bytes: u64,
    /// Files to copy.
    pub files: u64,
    root: PathBuf,
    pointer: Option<PathBuf>,
    to_default: bool,
    entries: Vec<String>,
    exclude: Vec<PathBuf>,
    config: StorageConfig,
}

/// How far a copy is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MoveProgress {
    /// Bytes copied.
    pub bytes: u64,
    /// Bytes to copy.
    pub total_bytes: u64,
    /// Files copied.
    pub files: u64,
    /// Files to copy.
    pub total_files: u64,
}

/// What a move did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveReport {
    /// The section.
    pub section: Section,
    /// The old folder, left as it was.
    pub from: PathBuf,
    /// The new folder.
    pub to: PathBuf,
    /// Bytes copied.
    pub bytes: u64,
    /// Files copied.
    pub files: u64,
    /// Links that were not copied.
    pub skipped_links: Vec<PathBuf>,
}

/// Checks a move request against `layout` and measures what it copies.
pub fn plan(
    env: &Env,
    layout: &Layout,
    unavailable: &[Section],
    request: &MoveRequest,
) -> Result<(MovePlan, PathCheck), StoreError> {
    let section = request.section;
    let pointer = if section == Section::Data {
        if layout.source().is_fixed() {
            return Err(StoreError::Conflict(
                "the data folder is chosen by --dir, VIBELAUNCHER_DIR or the portable marker; \
                 change it there"
                    .into(),
            ));
        }
        Some(
            paths::pointer_file(env)
                .ok_or_else(|| StoreError::NoDirectory("no folder for the pointer file".into()))?,
        )
    } else {
        None
    };
    let from = layout.path(section);
    let default_target = normalize(&if section == Section::Data {
        paths::default_root(env)?
    } else {
        layout.default_path(section)
    })?;
    let to = normalize(request.target.as_deref().unwrap_or(&default_target))?;
    if same(&to, &from) {
        return Err(StoreError::InvalidInput(format!(
            "{section} is already in {}",
            to.display()
        )));
    }
    if to.file_name().is_none() {
        return Err(StoreError::InvalidInput(format!(
            "{} is a drive root; choose a folder",
            to.display()
        )));
    }
    if request.mode == MoveMode::Copy && unavailable.contains(&section) {
        return Err(StoreError::Conflict(format!(
            "the folder of {section} ({}) is unavailable: reconnect its drive, or switch without \
             copying",
            from.display()
        )));
    }
    let exclude = exclusions(layout, section);
    let entries = top_entries(section, &from, &exclude);
    let mut measured = Walk::default();
    if request.mode == MoveMode::Copy {
        for entry in &entries {
            walk(
                &from.join(entry),
                None,
                &exclude,
                &mut measured,
                &mut |_| Ok(()),
            )?;
        }
    }
    let needed = (request.mode == MoveMode::Copy)
        .then(|| measured.bytes + measured.bytes / 20 + SPACE_MARGIN);
    let check = check_path(env, layout, section, &to, Some(request.mode), needed);
    if !check.usable() {
        let errors: Vec<String> = check
            .issues
            .iter()
            .filter(|i| i.is_error())
            .map(describe)
            .collect();
        return Err(StoreError::InvalidInput(format!(
            "{} cannot hold {section}: {}",
            to.display(),
            errors.join("; ")
        )));
    }
    Ok((
        MovePlan {
            section,
            from,
            to_default: same(&to, &default_target),
            to,
            mode: request.mode,
            bytes: measured.bytes,
            files: measured.files,
            root: layout.root().to_path_buf(),
            pointer,
            entries,
            exclude,
            config: layout.config().clone(),
        },
        check,
    ))
}

/// Runs a move. `progress` is called while copying; an error from it (such as
/// [`StoreError::Cancelled`]) stops the move and undoes it.
pub fn execute(
    plan: &MovePlan,
    progress: &mut dyn FnMut(&MoveProgress) -> Result<(), StoreError>,
) -> Result<MoveReport, StoreError> {
    let mut walked = Walk {
        total_bytes: plan.bytes,
        total_files: plan.files,
        ..Walk::default()
    };
    if plan.mode == MoveMode::Copy {
        copy_and_place(plan, &mut walked, progress)?;
    } else {
        switch(plan)?;
    }
    Ok(MoveReport {
        section: plan.section,
        from: plan.from.clone(),
        to: plan.to.clone(),
        bytes: walked.bytes,
        files: walked.files,
        skipped_links: walked.links,
    })
}

fn copy_and_place(
    plan: &MovePlan,
    walked: &mut Walk,
    progress: &mut dyn FnMut(&MoveProgress) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    let tmp = temp_dir(&plan.to)?;
    if tmp.exists() {
        fs::remove_dir_all(&tmp).map_err(|e| StoreError::io(&tmp, e))?;
    }
    let to_existed = plan.to.exists();
    let mut placed: Vec<String> = Vec::new();
    let result = (|| {
        fs::create_dir_all(&tmp).map_err(|e| StoreError::io(&tmp, e))?;
        for entry in &plan.entries {
            walk(
                &plan.from.join(entry),
                Some(&tmp.join(entry)),
                &plan.exclude,
                walked,
                progress,
            )?;
        }
        let mut copied = Walk::default();
        walk(&tmp, None, &[], &mut copied, &mut |_| Ok(()))?;
        if (copied.bytes, copied.files) != (walked.bytes, walked.files) {
            return Err(StoreError::corrupt(
                &tmp,
                format!(
                    "the copy holds {} files, {} bytes; {} files, {} bytes were copied",
                    copied.files, copied.bytes, walked.files, walked.bytes
                ),
            ));
        }
        if plan.section == Section::Data {
            let mut config = StorageConfig::load(&tmp)?;
            record_leftover(&mut config, plan);
            config.save(&tmp)?;
        }
        fs::create_dir_all(&plan.to).map_err(|e| StoreError::io(&plan.to, e))?;
        for entry in list(&tmp)? {
            let (src, dst) = (tmp.join(&entry), plan.to.join(&entry));
            fs::rename(&src, &dst).map_err(|e| StoreError::io(&dst, e))?;
            placed.push(entry);
        }
        switch(plan)
    })();
    if result.is_err() {
        for entry in &placed {
            let _ = fs::rename(plan.to.join(entry), tmp.join(entry));
        }
        if !to_existed {
            let _ = fs::remove_dir(&plan.to);
        }
    }
    let _ = fs::remove_dir_all(&tmp);
    result
}

/// Switches the path: the pointer for the data folder, `storage.json` for a section.
fn switch(plan: &MovePlan) -> Result<(), StoreError> {
    if plan.section == Section::Data {
        if plan.mode == MoveMode::SwitchOnly {
            let mut config = StorageConfig::load(&plan.to)?;
            record_leftover(&mut config, plan);
            config.save(&plan.to)?;
        }
        let Some(file) = &plan.pointer else {
            return Err(StoreError::NoDirectory(
                "no folder for the pointer file".into(),
            ));
        };
        return if plan.to_default {
            pointer::remove(file)
        } else {
            pointer::write(file, &plan.to, Some(&plan.from))
        };
    }
    let mut config = plan.config.clone();
    if plan.to_default {
        config.sections.remove(&plan.section);
    } else {
        config.sections.insert(plan.section, plan.to.clone());
    }
    record_leftover(&mut config, plan);
    config.save(&plan.root)
}

/// Forgets old copies at the new place (they are live again) and records the one left now.
fn record_leftover(config: &mut StorageConfig, plan: &MovePlan) {
    config
        .leftovers
        .retain(|l| !is_within(&l.path, &plan.to) && !is_within(&plan.to, &l.path));
    let mut entries = plan.entries.clone();
    if plan.section == Section::Data && plan.from.join(lock::LOCK_FILE).exists() {
        entries.push(lock::LOCK_FILE.into());
    }
    if plan.from.exists() && !entries.is_empty() {
        config.leftovers.push(LeftoverRecord {
            section: plan.section,
            path: plan.from.clone(),
            entries,
        });
    }
}

/// What a move must not copy: the lock, other sections' explicit folders, old copies.
fn exclusions(layout: &Layout, section: Section) -> Vec<PathBuf> {
    let mut exclude = vec![layout.root().join(lock::LOCK_FILE)];
    for other in Section::ALL {
        if other != section && !layout.is_default(other) {
            exclude.extend(layout.owned(other));
        }
    }
    for leftover in &layout.config().leftovers {
        exclude.extend(leftover.entries.iter().map(|e| leftover.path.join(e)));
    }
    exclude
}

/// The top-level names a move of `section` takes from `from`.
fn top_entries(section: Section, from: &Path, exclude: &[PathBuf]) -> Vec<String> {
    let names = match section {
        Section::Game => GAME_ENTRIES.iter().map(|e| (*e).to_owned()).collect(),
        _ => list(from).unwrap_or_default(),
    };
    names
        .into_iter()
        .filter(|name| {
            let path = from.join(name);
            path.exists()
                && !name.ends_with(MOVING_SUFFIX)
                && !exclude.iter().any(|x| same(x, &path))
        })
        .collect()
}

fn list(dir: &Path) -> Result<Vec<String>, StoreError> {
    let mut names = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| StoreError::io(dir, e))? {
        let entry = entry.map_err(|e| StoreError::io(dir, e))?;
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    Ok(names)
}

/// `.<name>.vl-moving` next to `to`.
fn temp_dir(to: &Path) -> Result<PathBuf, StoreError> {
    let (Some(parent), Some(name)) = (to.parent(), to.file_name()) else {
        return Err(StoreError::InvalidInput(format!(
            "{} is a drive root; choose a folder",
            to.display()
        )));
    };
    Ok(parent.join(format!(".{}{MOVING_SUFFIX}", name.to_string_lossy())))
}

fn same(a: &Path, b: &Path) -> bool {
    is_within(a, b) && is_within(b, a)
}

/// Counters of a walk.
#[derive(Debug, Default)]
struct Walk {
    bytes: u64,
    files: u64,
    total_bytes: u64,
    total_files: u64,
    reported: u64,
    links: Vec<PathBuf>,
}

impl Walk {
    fn report(
        &mut self,
        progress: &mut dyn FnMut(&MoveProgress) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.reported = self.bytes;
        progress(&MoveProgress {
            bytes: self.bytes,
            total_bytes: self.total_bytes.max(self.bytes),
            files: self.files,
            total_files: self.total_files.max(self.files),
        })
    }
}

/// Counts `src`, or copies it to `dst`, skipping `exclude` and links.
fn walk(
    src: &Path,
    dst: Option<&Path>,
    exclude: &[PathBuf],
    state: &mut Walk,
    progress: &mut dyn FnMut(&MoveProgress) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    if exclude.iter().any(|x| same(x, src)) {
        return Ok(());
    }
    let meta = fs::symlink_metadata(src).map_err(|e| StoreError::io(src, e))?;
    if meta.file_type().is_symlink() {
        state.links.push(src.to_path_buf());
        return Ok(());
    }
    if meta.is_dir() {
        if let Some(dst) = dst {
            fs::create_dir_all(dst).map_err(|e| StoreError::io(dst, e))?;
        }
        for name in list(src)? {
            walk(
                &src.join(&name),
                dst.map(|d| d.join(&name)).as_deref(),
                exclude,
                state,
                progress,
            )?;
        }
        return Ok(());
    }
    match dst {
        Some(dst) => copy_file(src, dst, &meta, state, progress)?,
        None => {
            state.bytes += meta.len();
            state.files += 1;
        }
    }
    Ok(())
}

fn copy_file(
    src: &Path,
    dst: &Path,
    meta: &fs::Metadata,
    state: &mut Walk,
    progress: &mut dyn FnMut(&MoveProgress) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    let mut input = File::open(src).map_err(|e| StoreError::io(src, e))?;
    let mut output = File::create_new(dst).map_err(|e| StoreError::io(dst, e))?;
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = input
            .read(&mut buffer)
            .map_err(|e| StoreError::io(src, e))?;
        if n == 0 {
            break;
        }
        output
            .write_all(&buffer[..n])
            .map_err(|e| StoreError::io(dst, e))?;
        state.bytes += n as u64;
        if state.bytes - state.reported >= REPORT_EVERY {
            state.report(progress)?;
        }
    }
    output.flush().map_err(|e| StoreError::io(dst, e))?;
    if let Ok(modified) = meta.modified() {
        let _ = output.set_modified(modified);
    }
    #[cfg(unix)]
    fs::set_permissions(dst, meta.permissions()).map_err(|e| StoreError::io(dst, e))?;
    state.files += 1;
    state.report(progress)
}

/// One line for a path issue, for error messages and the command line.
pub fn describe(issue: &PathIssue) -> String {
    match issue {
        PathIssue::NotWritable { detail } => format!("cannot write there ({detail})"),
        PathIssue::ProgramFiles => "inside the folder for installed programs".into(),
        PathIssue::SystemFolder => "inside an operating system folder".into(),
        PathIssue::SystemDriveRoot => "the root of the system drive".into(),
        PathIssue::Overlaps { section } => format!("overlaps the folder of {section}"),
        PathIssue::NotEmpty => "the folder is not empty".into(),
        PathIssue::Missing => "the folder does not hold the data yet".into(),
        PathIssue::NotEnoughSpace { free, needed } => {
            format!("not enough space: {free} bytes free, {needed} needed")
        }
        PathIssue::CloudFolder { service } => {
            format!("synchronized by {service}: files may be locked or held online only")
        }
        PathIssue::NetworkDrive => "a network drive: slower, and the lock is not guaranteed".into(),
        PathIssue::RemovableDrive => {
            "a removable drive: the data is missing while it is out".into()
        }
        PathIssue::LongPath { length, limit } => format!(
            "a long path ({length} characters, more than {limit}): files deep inside may pass the \
             Windows limit"
        ),
        PathIssue::NonAscii => {
            "characters outside ASCII: some Java versions and mods mishandle them".into()
        }
        PathIssue::LowSpace { free } => format!("little free space ({free} bytes)"),
        _ => format!("{issue:?}"),
    }
}

/// Removes the old copies recorded in `storage.json` and, outside portable mode, the cache and
/// logs folders of earlier versions; then prunes logs and the cache. A copy that overlaps a folder
/// in use is never touched and stays listed.
pub fn cleanup(env: &Env, layout: &Layout, policy: &Policy) -> Result<CleanupReport, StoreError> {
    let mut report = CleanupReport::default();
    let mut config = StorageConfig::load(layout.root())?;
    let mut kept = Vec::new();
    for mut record in std::mem::take(&mut config.leftovers) {
        record.entries.retain(|entry| {
            let path = record.path.join(entry);
            if fs::symlink_metadata(&path).is_err() {
                return false;
            }
            !remove(layout, &path, &mut report)
        });
        if record.entries.is_empty() {
            if record.path.parent().is_some() && !layout.is_live(&record.path) {
                let _ = fs::remove_dir(&record.path);
            }
        } else {
            kept.push(record);
        }
    }
    config.leftovers = kept;
    config.save(layout.root())?;
    if !layout.source().is_fixed() {
        for (_, dir) in paths::legacy_dirs(env) {
            if dir.exists() {
                remove(layout, &dir, &mut report);
            }
        }
    }
    for (section, limits) in [(Section::Logs, policy.logs), (Section::Cache, policy.cache)] {
        report.freed += retention::prune(&layout.path(section), limits).bytes;
    }
    Ok(report)
}

/// Removes `path` unless it is live; whether it was removed.
fn remove(layout: &Layout, path: &Path, report: &mut CleanupReport) -> bool {
    if layout.is_live(path) {
        report
            .failed
            .push((path.to_path_buf(), "overlaps a folder in use".into()));
        return false;
    }
    let size = retention::size_of(path);
    let result = match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) => Err(e),
    };
    match result {
        Ok(()) => {
            report.removed.push(path.to_path_buf());
            report.freed += size;
            true
        }
        Err(e) => {
            report.failed.push((path.to_path_buf(), e.to_string()));
            false
        }
    }
}
