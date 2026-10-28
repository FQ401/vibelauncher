//! Carrying out a plan: download with checks, then add to the folders, all or nothing.

use std::fs;
use std::path::{Path, PathBuf};

use vl_core::install::{InstallPlan, PlanAction, PlanItem};
use vl_core::resources::ResourceKind;
use vl_core::{Error, ErrorKind, Result};
use vl_net::{Checksum, Client, Control, Download};
use vl_resources::{DISABLED_SUFFIX, Resources};

use crate::hashing::hash_file;
use crate::origin::{Origin, OriginStore, now_unix};

/// Where an install writes.
pub struct ApplyInput<'a> {
    /// Downloads.
    pub client: &'a Client,
    /// The instance's resource folders.
    pub resources: &'a Resources,
    /// The origin records of the instance.
    pub origins: &'a OriginStore,
    /// A scratch directory for downloads; removed afterwards.
    pub staging: &'a Path,
}

/// What an install did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    /// Files added: `(folder, file name)`.
    pub added: Vec<(ResourceKind, String)>,
    /// Files moved to the trash because a new version took their place.
    pub replaced: Vec<(ResourceKind, String)>,
    /// Things that went wrong without undoing the install.
    pub warnings: Vec<String>,
}

struct Staging(PathBuf);

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// What was done to the folders, to undo it when a later step fails.
enum Step {
    Added { kind: ResourceKind, file: String },
    Trashed { from: PathBuf, trash: PathBuf },
}

/// The checksum a file is verified against: SHA-512 when the platform gives it.
fn checksum(item: &PlanItem) -> Result<Checksum> {
    item.file
        .sha512
        .as_deref()
        .and_then(Checksum::sha512)
        .or_else(|| item.file.sha1.as_deref().and_then(Checksum::sha1))
        .ok_or_else(|| {
            Error::new(
                ErrorKind::Integrity,
                format!("{} has no usable hash", item.file.filename),
            )
        })
}

/// Downloads what the plan needs and adds it to the instance. Either every file is added (old
/// versions go to the trash) or the folders are left as they were.
///
/// # Errors
///
/// `Network`, `Integrity` (size or hash mismatch), `Cancelled`, `Conflict` (a name appeared
/// since the plan was made) or `Io`.
pub fn execute(input: &ApplyInput<'_>, plan: &InstallPlan, control: &Control) -> Result<Report> {
    let todo: Vec<&PlanItem> = plan.items.iter().filter(|i| i.downloads()).collect();
    let mut report = Report::default();
    if todo.is_empty() {
        return Ok(report);
    }
    fs::create_dir_all(input.staging).map_err(|e| io(input.staging, &e))?;
    let _staging = Staging(input.staging.to_path_buf());

    let mut specs = Vec::new();
    let mut staged = Vec::new();
    for (n, item) in todo.iter().enumerate() {
        let dest = input.staging.join(n.to_string()).join(&item.file.filename);
        let mut spec = Download::new(&item.file.url, &dest).checksum(checksum(item)?);
        if item.file.size > 0 {
            spec = spec.size(item.file.size);
        }
        specs.push(spec);
        staged.push(dest);
    }
    for result in input.client.download_all(&specs, control) {
        result.map_err(Error::from)?;
    }
    if control.cancel.is_cancelled() {
        return Err(Error::new(ErrorKind::Cancelled, "install cancelled"));
    }

    let mut journal: Vec<Step> = Vec::new();
    let mut added = Vec::new();
    let mut removed = Vec::new();
    for (item, source) in todo.iter().zip(&staged) {
        if let Err(e) = apply_one(input, item, source, &mut journal, &mut added, &mut removed) {
            rollback(input.resources, journal);
            return Err(e);
        }
    }

    for (kind, file) in &removed {
        report.replaced.push((*kind, file.clone()));
    }
    report
        .added
        .extend(added.iter().map(|o: &Origin| (o.kind, o.file.clone())));
    let resources = input.resources;
    let recorded = input.origins.record(
        added,
        |o| {
            let dir = resources.dir(o.kind);
            dir.join(&o.file).exists() || dir.join(format!("{}{DISABLED_SUFFIX}", o.file)).exists()
        },
        &removed,
    );
    if let Err(e) = recorded {
        report.warnings.push(format!(
            "the files are installed but their origin could not be recorded: {}",
            e.message
        ));
    }
    Ok(report)
}

fn apply_one(
    input: &ApplyInput<'_>,
    item: &PlanItem,
    source: &Path,
    journal: &mut Vec<Step>,
    added: &mut Vec<Origin>,
    removed: &mut Vec<(ResourceKind, String)>,
) -> Result<()> {
    let resources = input.resources;
    let dir = resources.dir(item.kind);
    let mut was_disabled = false;
    let mut was_dependency = false;
    if let PlanAction::Replace { file, .. } = &item.action {
        was_dependency = input
            .origins
            .load()
            .origins
            .iter()
            .any(|o| o.kind == item.kind && o.file == *file && o.dependency);
        let disabled = dir.join(format!("{file}{DISABLED_SUFFIX}"));
        was_disabled = disabled.exists();
        let from = if was_disabled {
            disabled
        } else {
            dir.join(file)
        };
        let trash = resources.remove(item.kind, file)?;
        journal.push(Step::Trashed { from, trash });
        removed.push((item.kind, file.clone()));
    }
    let name = resources.add(item.kind, source)?;
    journal.push(Step::Added {
        kind: item.kind,
        file: name.clone(),
    });
    if was_disabled {
        resources.set_enabled(item.kind, &name, false)?;
    }
    let hashes = hash_file(&dir.join(if was_disabled {
        format!("{name}{DISABLED_SUFFIX}")
    } else {
        name.clone()
    }))
    .map_err(|e| io(&dir, &e))?;
    added.push(Origin {
        kind: item.kind,
        file: name,
        sha1: hashes.sha1,
        sha512: Some(hashes.sha512),
        size: hashes.size,
        provider: item.provider,
        project_id: item.project_id.clone(),
        slug: Some(item.slug.clone()),
        version_id: item.version_id.clone(),
        version_number: Some(item.version_number.clone()),
        installed_unix: now_unix(),
        dependency: was_dependency
            || matches!(item.role, vl_core::install::PlanRole::Dependency { .. }),
    });
    Ok(())
}

/// Undoes the steps, newest first; what cannot be undone is left (the trash keeps old files).
fn rollback(resources: &Resources, journal: Vec<Step>) {
    for step in journal.into_iter().rev() {
        match step {
            Step::Added { kind, file } => {
                let dir = resources.dir(kind);
                let _ = fs::remove_file(dir.join(&file));
                let _ = fs::remove_file(dir.join(format!("{file}{DISABLED_SUFFIX}")));
            }
            Step::Trashed { from, trash } => {
                let _ = fs::rename(trash, from);
            }
        }
    }
}

fn io(path: &Path, e: &std::io::Error) -> Error {
    Error::new(ErrorKind::Io, format!("{}: {e}", path.display()))
}
