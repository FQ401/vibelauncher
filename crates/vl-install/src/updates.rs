//! Origins of hand-added files, update checks and updates.
//!
//! [`identify`] finds, by hash, which catalog project a file in a resource folder belongs to
//! and records it like an install would. [`check`] looks for newer versions of every mod, resource
//! pack, shader pack and data pack of an instance: one batched lookup of all file hashes, then one
//! batched "newest version for these files" request for the mods (filtered by game version,
//! loader and release channel) and one for the other kinds. Only a version that is newer than the
//! installed one is offered, so an update never goes back to an older version.
//!
//! Every operation works on one platform and finds files by that platform's hash
//! ([`CatalogSource::file_hash`]: SHA-1 on Modrinth, the fingerprint on CurseForge). A file
//! whose origin record names another platform belongs to that platform: a check does not
//! offer updates for it and [`identify`] keeps its record. [`check_all`] and
//! [`plan_update_all`] work on several platforms at once and give each file to one of them: the
//! platform of its origin record, else the first that knows it.
//!
//! Files that came with the instance's modpack are left out unless asked for: the pack is
//! updated as a whole. [`plan_update`] turns a selection into an [`InstallPlan`] through the same
//! planner as an install, so new required dependencies are resolved the same way; the plan is
//! carried out by [`crate::execute`], which replaces files all together or not at all and keeps
//! disabled files disabled. Switching a file to a chosen version, older ones included, is the
//! same plan with an explicit version.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use vl_core::catalog::{ProjectVersion, Provider, ReleaseChannel, VersionFilter};
use vl_core::install::{
    IdentifiedFile, IdentifyReport, InstallPlan, MatchedBy, MinStability, PackFile, UncheckedFile,
    UncheckedReason, UpdateCandidate, UpdateConflict, UpdateReport, VersionLabel,
};
use vl_core::resources::ResourceKind;
use vl_core::{Error, ErrorKind, Result};
use vl_net::Control;
use vl_platforms::CatalogSource;
use vl_resources::{DISABLED_SUFFIX, Resources};

use crate::mrpack::PackRecord;
use crate::origin::{Origin, OriginStore, now_unix};
use crate::plan::{PlanInput, Planner, Target};
use crate::scan::{Identity, InstalledFile, normalize_id, scan_many};

/// The folders that are checked. Only the files directly in a folder count: the game loads
/// nothing from a subfolder of these, and a pack unpacked into a folder has no file to compare.
const KINDS: [ResourceKind; 5] = [
    ResourceKind::Mods,
    ResourceKind::ResourcePacks,
    ResourceKind::ShaderPacks,
    ResourceKind::DataPacks,
    ResourceKind::GlobalDataPacks,
];

/// The only folder looked at in a world.
const WORLD_KINDS: [ResourceKind; 1] = [ResourceKind::DataPacks];

/// What the operations here work on.
pub struct UpdateInput<'a> {
    /// The platform.
    pub source: &'a dyn CatalogSource,
    /// The instance's resource folders.
    pub resources: &'a Resources,
    /// The origin records of the instance.
    pub origins: &'a OriginStore,
    /// The instance's game version and loader.
    pub target: &'a Target,
    /// The launcher's folder inside the instance (`pack.json` lives there).
    pub meta_dir: &'a Path,
    /// Cancellation.
    pub control: &'a Control,
}

/// How an update check chooses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateOptions {
    /// The least stable channel an update may come from.
    pub min: MinStability,
    /// Widen `min`, file by file, to the channel of the installed version
    /// ([`MinStability::allowing`]): a file installed from a beta is offered newer betas. On
    /// when `min` is the stored setting; off when the caller named a channel.
    pub follow_installed: bool,
    /// Also handle files that came with the modpack.
    pub include_pack_files: bool,
}

impl Default for UpdateOptions {
    fn default() -> Self {
        Self {
            min: MinStability::Release,
            follow_installed: false,
            include_pack_files: false,
        }
    }
}

impl UpdateOptions {
    /// The least stable channel an update of a file at `installed` may come from.
    pub fn floor(&self, installed: Option<ReleaseChannel>) -> MinStability {
        match installed {
            Some(channel) if self.follow_installed => self.min.allowing(channel),
            _ => self.min,
        }
    }
}

/// Which files an update touches.
#[derive(Debug, Clone, Copy)]
pub enum Selection<'a> {
    /// Every file that has an update.
    All,
    /// These: file names, project ids, mod ids or names.
    Files(&'a [String]),
}

/// An update check with the data the plan needs.
struct Inspection {
    report: UpdateReport,
    scanned: HashMap<ResourceKind, Vec<InstalledFile>>,
    /// The newer versions found, by version id.
    latest: HashMap<String, ProjectVersion>,
    pack_files: HashSet<(ResourceKind, String)>,
    /// What the check says about each file, in folder order.
    outcomes: Vec<(FileKey, Outcome)>,
}

/// A file of a folder: the folder and the name without `.disabled`.
type FileKey = (ResourceKind, String);

/// What one platform's check says about one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// A newer version exists (the candidate is in the report).
    Update,
    /// On the newest allowed version.
    UpToDate,
    /// Left out because it came with the modpack.
    FromPack,
    /// Not checked.
    Unchecked(UncheckedReason),
}

impl Outcome {
    /// Whether the platform takes the file: it knows it, or it has the file's origin record.
    /// A file nobody knows, or one recorded as another platform's, is not taken.
    fn taken(self) -> bool {
        !matches!(
            self,
            Self::Unchecked(UncheckedReason::NoOrigin | UncheckedReason::OtherPlatform)
        )
    }
}

fn pack_hashes(meta_dir: &Path) -> HashSet<String> {
    PackRecord::load(meta_dir)
        .map(|p| {
            p.files
                .iter()
                .flat_map(|f| [f.sha1.to_ascii_lowercase(), f.sha512.to_ascii_lowercase()])
                .collect()
        })
        .unwrap_or_default()
}

fn channels_for(min: MinStability) -> Vec<ReleaseChannel> {
    match min {
        MinStability::Release => vec![ReleaseChannel::Release],
        MinStability::Beta => vec![ReleaseChannel::Release, ReleaseChannel::Beta],
        _ => Vec::new(),
    }
}

fn label(v: &ProjectVersion) -> VersionLabel {
    VersionLabel {
        id: v.id.clone(),
        number: v.version_number.clone(),
        channel: v.channel,
    }
}

/// Whether `new` is a real update of `current` for this instance. The platform's answer is data:
/// it is checked to be newer, to fit and to be of an allowed channel. A pack or shader whose
/// installed version names no loader (CurseForge files often name none) may be updated by one
/// that names none either.
fn is_update(
    kind: ResourceKind,
    current: &ProjectVersion,
    new: &ProjectVersion,
    target: &Target,
    loaders: &[String],
    min: MinStability,
) -> bool {
    let unlabeled =
        kind != ResourceKind::Mods && current.loaders.is_empty() && new.loaders.is_empty();
    new.id != current.id
        && new.project_id == current.project_id
        && new.date_published > current.date_published
        && new.game_versions.contains(&target.game_version)
        && (loaders.is_empty() || unlabeled || new.loaders.iter().any(|l| loaders.contains(l)))
        && min.rank(new.channel).is_some()
}

/// A file to ask the platform about.
struct Asked<'a> {
    kind: ResourceKind,
    file: &'a InstalledFile,
    current: &'a ProjectVersion,
    /// The least stable channel its update may come from.
    min: MinStability,
    /// Its place in the outcomes.
    slot: usize,
}

/// A file with an origin record that the platform does not find by hash (its version was
/// deleted, or its file is not indexed): its project's versions are listed instead.
struct Orphan<'a> {
    kind: ResourceKind,
    file: &'a InstalledFile,
    identity: &'a Identity,
    /// Its place in the outcomes.
    slot: usize,
}

/// The folders to look at: a world has only data packs.
fn kinds(target: &Target) -> &'static [ResourceKind] {
    if target.world { &WORLD_KINDS } else { &KINDS }
}

/// The loaders whose mods run in the instance, its own first. Quilt runs Fabric mods.
fn mod_loaders(target: &Target) -> Vec<String> {
    match target.loader.as_deref() {
        Some("quilt") => vec!["quilt".to_owned(), "fabric".to_owned()],
        Some(other) => vec![other.to_owned()],
        None => Vec::new(),
    }
}

/// Projects with more than one file in a folder, among the files this platform knows.
fn same_project(
    scanned: &HashMap<ResourceKind, Vec<InstalledFile>>,
    kinds: &[ResourceKind],
    provider: Provider,
) -> Vec<UpdateConflict> {
    let mut conflicts = Vec::new();
    for &kind in kinds {
        let mut by_project: Vec<(&str, Vec<String>)> = Vec::new();
        for file in scanned.get(&kind).map_or(&[][..], Vec::as_slice) {
            let Some(identity) = file.identity.as_ref().filter(|_| file.foreign.is_none()) else {
                continue;
            };
            match by_project
                .iter_mut()
                .find(|(p, _)| *p == identity.project_id)
            {
                Some((_, files)) => files.push(file.file.clone()),
                None => by_project.push((&identity.project_id, vec![file.file.clone()])),
            }
        }
        for (project_id, mut files) in by_project {
            if files.len() > 1 {
                files.sort();
                conflicts.push(UpdateConflict {
                    kind,
                    provider,
                    project_id: project_id.to_owned(),
                    files,
                });
            }
        }
    }
    conflicts
}

fn conflict_of<'r>(
    conflicts: &'r [UpdateConflict],
    kind: ResourceKind,
    file: &str,
) -> Option<&'r UpdateConflict> {
    conflicts
        .iter()
        .find(|c| c.kind == kind && c.files.iter().any(|f| f == file))
}

fn inspect(input: &UpdateInput<'_>, options: UpdateOptions) -> Result<Inspection> {
    let loaded = input.origins.load();
    let mut report = UpdateReport {
        game_version: input.target.game_version.clone(),
        loader: input.target.loader.clone(),
        ..UpdateReport::default()
    };
    if loaded.damaged {
        report.warnings.push(format!(
            "the origin records {} are damaged and were partly ignored",
            input.origins.path().display()
        ));
    }
    let scanned = scan_many(
        input.resources,
        kinds(input.target),
        &loaded.origins,
        input.source,
        input.control,
    )?;
    let pack = pack_hashes(input.meta_dir);
    let mut pack_files = HashSet::new();
    let mut outcomes: Vec<(FileKey, Outcome)> = Vec::new();
    report.conflicts = same_project(&scanned, kinds(input.target), input.source.provider());

    let mut mods: Vec<Asked<'_>> = Vec::new();
    let mut others: Vec<Asked<'_>> = Vec::new();
    let mut orphans: Vec<Orphan<'_>> = Vec::new();
    for &kind in kinds(input.target) {
        for file in scanned.get(&kind).map_or(&[][..], Vec::as_slice) {
            let key = (kind, file.file.clone());
            let mut unchecked = |reason| {
                report.unchecked.push(UncheckedFile {
                    kind,
                    file: file.file.clone(),
                    reason,
                });
                outcomes.push((key.clone(), Outcome::Unchecked(reason)));
            };
            // A file installed from another platform is checked there.
            if file.foreign.is_some() {
                unchecked(UncheckedReason::OtherPlatform);
                continue;
            }
            // Two files of one project: an update would replace only one of them.
            if conflict_of(&report.conflicts, kind, &file.file).is_some() {
                unchecked(UncheckedReason::SameProject);
                continue;
            }
            if pack.contains(&file.hashes.sha1) || pack.contains(&file.hashes.sha512) {
                pack_files.insert(key.clone());
                if !options.include_pack_files {
                    report.from_pack.push(PackFile {
                        kind,
                        file: file.file.clone(),
                    });
                    outcomes.push((key, Outcome::FromPack));
                    continue;
                }
            }
            let mut unchecked = |reason| {
                report.unchecked.push(UncheckedFile {
                    kind,
                    file: file.file.clone(),
                    reason,
                });
                outcomes.push((key.clone(), Outcome::Unchecked(reason)));
            };
            let Some(identity) = file.identity.as_ref() else {
                unchecked(UncheckedReason::NoOrigin);
                continue;
            };
            if kind == ResourceKind::Mods && input.target.loader.is_none() {
                unchecked(UncheckedReason::NoLoader);
                continue;
            }
            let slot = outcomes.len();
            // Up to date until the answer says otherwise.
            outcomes.push((key, Outcome::UpToDate));
            let Some(current) = identity.version.as_ref() else {
                orphans.push(Orphan {
                    kind,
                    file,
                    identity,
                    slot,
                });
                continue;
            };
            let asked = Asked {
                kind,
                file,
                current,
                min: options.floor(Some(current.channel)),
                slot,
            };
            if kind == ResourceKind::Mods {
                mods.push(asked);
            } else {
                others.push(asked);
            }
        }
    }

    let mut latest: HashMap<String, ProjectVersion> = HashMap::new();
    let mut found: Vec<(usize, UpdateCandidate)> = Vec::new();
    let mut up_to_date = 0u32;

    let mod_loaders = mod_loaders(input.target);
    // Packs and shaders name their own loaders (`minecraft`, `iris`, ...): the request allows
    // the ones the installed files have.
    let mut other_loaders: Vec<String> = others
        .iter()
        .flat_map(|a| a.current.loaders.iter().cloned())
        .collect();
    other_loaders.sort();
    other_loaders.dedup();

    for (group, loaders) in [(&mods, &mod_loaders), (&others, &other_loaders)] {
        // One request per channel floor in use (one, unless installed versions widen it).
        let mut floors: Vec<MinStability> = Vec::new();
        for asked in group.iter() {
            if !floors.contains(&asked.min) {
                floors.push(asked.min);
            }
        }
        for min in floors {
            let batch: Vec<&Asked<'_>> = group.iter().filter(|a| a.min == min).collect();
            let hashes: Vec<String> = batch.iter().map(|a| a.file.key.clone()).collect();
            let filter = VersionFilter {
                loaders: loaders.clone(),
                game_versions: vec![input.target.game_version.clone()],
            };
            let answers: HashMap<String, ProjectVersion> = input
                .source
                .latest_versions_by_hashes(
                    input.source.file_hash(),
                    &hashes,
                    &filter,
                    &channels_for(min),
                    input.control,
                )?
                .into_iter()
                .map(|m| (m.hash.to_ascii_lowercase(), m.version))
                .collect();
            for asked in batch {
                match answers.get(&asked.file.key) {
                    Some(new)
                        if is_update(
                            asked.kind,
                            asked.current,
                            new,
                            input.target,
                            loaders,
                            asked.min,
                        ) =>
                    {
                        latest.insert(new.id.clone(), new.clone());
                        if let Some(slot) = outcomes.get_mut(asked.slot) {
                            slot.1 = Outcome::Update;
                        }
                        found.push((
                            asked.slot,
                            candidate(
                                input,
                                &pack_files,
                                asked.kind,
                                asked.file,
                                asked.current,
                                new,
                            ),
                        ));
                    }
                    _ => up_to_date += 1,
                }
            }
        }
    }

    // Files the platform no longer finds by hash: their projects' versions, once per project.
    let mut listed: HashMap<(bool, String), Vec<ProjectVersion>> = HashMap::new();
    for orphan in &orphans {
        let is_mod = orphan.kind == ResourceKind::Mods;
        let project = orphan.identity.project_id.clone();
        if let std::collections::hash_map::Entry::Vacant(slot) =
            listed.entry((is_mod, project.clone()))
        {
            let filter = VersionFilter {
                loaders: if is_mod {
                    mod_loaders.clone()
                } else {
                    Vec::new()
                },
                game_versions: vec![input.target.game_version.clone()],
            };
            let versions = match input.source.versions(&project, &filter, input.control) {
                Ok(v) => v,
                // The project is gone too.
                Err(e) if e.kind == ErrorKind::NotFound => Vec::new(),
                Err(e) => return Err(e),
            };
            slot.insert(versions);
        }
        let versions = listed
            .get(&(is_mod, project))
            .map_or(&[][..], Vec::as_slice);
        let loaders = if is_mod { &mod_loaders[..] } else { &[][..] };
        match newest_for_orphan(orphan, versions, input.target, loaders, options) {
            OrphanAnswer::Update(update) => {
                let OrphanUpdate { current, new } = *update;
                latest.insert(new.id.clone(), new.clone());
                if let Some(slot) = outcomes.get_mut(orphan.slot) {
                    slot.1 = Outcome::Update;
                }
                found.push((
                    orphan.slot,
                    candidate(input, &pack_files, orphan.kind, orphan.file, &current, &new),
                ));
            }
            OrphanAnswer::UpToDate => up_to_date += 1,
            OrphanAnswer::Unknown => {
                let reason = UncheckedReason::UnknownToPlatform;
                report.unchecked.push(UncheckedFile {
                    kind: orphan.kind,
                    file: orphan.file.file.clone(),
                    reason,
                });
                if let Some(slot) = outcomes.get_mut(orphan.slot) {
                    slot.1 = Outcome::Unchecked(reason);
                }
            }
        }
    }

    found.sort_by_key(|(slot, _)| *slot);
    report.updates = found.into_iter().map(|(_, c)| c).collect();
    report.up_to_date = up_to_date;
    Ok(Inspection {
        report,
        scanned,
        latest,
        pack_files,
        outcomes,
    })
}

/// The report line of a file with a newer version.
fn candidate(
    input: &UpdateInput<'_>,
    pack_files: &HashSet<FileKey>,
    kind: ResourceKind,
    file: &InstalledFile,
    current: &ProjectVersion,
    new: &ProjectVersion,
) -> UpdateCandidate {
    UpdateCandidate {
        kind,
        file: file.file.clone(),
        enabled: file.enabled,
        name: file.name.clone(),
        provider: input.source.provider(),
        project_id: current.project_id.clone(),
        current: label(current),
        latest: label(new),
        from_pack: pack_files.contains(&(kind, file.file.clone())),
    }
}

/// The update of an orphan.
struct OrphanUpdate {
    /// The installed version as far as it is known.
    current: ProjectVersion,
    /// The version to update to.
    new: ProjectVersion,
}

/// What the project's version list says about a file the platform does not find by hash.
enum OrphanAnswer {
    /// A version to update to.
    Update(Box<OrphanUpdate>),
    /// The installed version is listed and nothing allowed is newer.
    UpToDate,
    /// No version of the project fits the instance (or the project is gone).
    Unknown,
}

/// The update of an orphan: the newest version that fits the instance and an allowed channel
/// (the instance's own loader first). While the installed version is still listed it is
/// compared as usual, so nothing older is offered; once it is gone its date is unknown, and the
/// newest fitting version replaces it.
fn newest_for_orphan(
    orphan: &Orphan<'_>,
    versions: &[ProjectVersion],
    target: &Target,
    loaders: &[String],
    options: UpdateOptions,
) -> OrphanAnswer {
    let project = &orphan.identity.project_id;
    let listed_current = versions
        .iter()
        .find(|v| v.id == orphan.identity.version_id && v.project_id == *project);
    let min = options.floor(listed_current.map(|v| v.channel));
    let primary = loaders.first();
    let best = versions
        .iter()
        .filter(|v| v.project_id == *project && v.id != orphan.identity.version_id)
        .filter(|v| v.game_versions.contains(&target.game_version))
        .filter(|v| loaders.is_empty() || v.loaders.iter().any(|l| loaders.contains(l)))
        .filter(|v| min.rank(v.channel).is_some())
        .filter(|v| listed_current.is_none_or(|c| v.date_published > c.date_published))
        .max_by(|a, b| {
            let native = |v: &ProjectVersion| primary.is_some_and(|l| v.loaders.contains(l));
            (native(a), &a.date_published).cmp(&(native(b), &b.date_published))
        });
    match (best, listed_current) {
        (Some(new), current) => OrphanAnswer::Update(Box::new(OrphanUpdate {
            current: current.cloned().unwrap_or_else(|| ProjectVersion {
                id: orphan.identity.version_id.clone(),
                project_id: project.clone(),
                name: String::new(),
                version_number: orphan.identity.version_number.clone().unwrap_or_default(),
                channel: ReleaseChannel::Unknown,
                game_versions: Vec::new(),
                loaders: Vec::new(),
                date_published: String::new(),
                downloads: 0,
                files: Vec::new(),
                dependencies: Vec::new(),
            }),
            new: new.clone(),
        })),
        (None, Some(_)) => OrphanAnswer::UpToDate,
        (None, None) => OrphanAnswer::Unknown,
    }
}

/// Looks for newer versions of the instance's mods, packs and shaders.
///
/// # Errors
///
/// `Network`, `Cancelled`, `Io`, or `Unsupported` when the platform cannot answer update lookups.
pub fn check(input: &UpdateInput<'_>, options: UpdateOptions) -> Result<UpdateReport> {
    Ok(inspect(input, options)?.report)
}

/// Finds the origin of files that have no record and writes it to the instance's records.
///
/// # Errors
///
/// `Network`, `Cancelled`, `Io`.
pub fn identify(input: &UpdateInput<'_>) -> Result<IdentifyReport> {
    let loaded = input.origins.load();
    let scanned = scan_many(
        input.resources,
        kinds(input.target),
        &loaded.origins,
        input.source,
        input.control,
    )?;
    let mut report = IdentifyReport::default();
    if loaded.damaged {
        report.warnings.push(format!(
            "the origin records {} are damaged and were partly ignored",
            input.origins.path().display()
        ));
    }
    let provider = input.source.provider();
    let mut added = Vec::new();
    for &kind in kinds(input.target) {
        for file in scanned.get(&kind).map_or(&[][..], Vec::as_slice) {
            match &file.identity {
                // Recorded as installed from another platform: that record stays.
                _ if file.foreign.is_some() => report.already_known += 1,
                None => report.unknown.push(PackFile {
                    kind,
                    file: file.file.clone(),
                }),
                Some(i) if i.matched_by == MatchedBy::Origin => report.already_known += 1,
                Some(i) => {
                    added.push(Origin {
                        kind,
                        file: file.file.clone(),
                        sha1: file.hashes.sha1.clone(),
                        sha512: Some(file.hashes.sha512.clone()),
                        size: file.hashes.size,
                        provider,
                        project_id: i.project_id.clone(),
                        slug: None,
                        version_id: i.version_id.clone(),
                        version_number: i.version_number.clone(),
                        installed_unix: now_unix(),
                        dependency: false,
                    });
                    report.identified.push(IdentifiedFile {
                        kind,
                        file: file.file.clone(),
                        project_id: i.project_id.clone(),
                        version: VersionLabel {
                            id: i.version_id.clone(),
                            number: i.version_number.clone().unwrap_or_default(),
                            channel: i.version.as_ref().map(|v| v.channel).unwrap_or_default(),
                        },
                    });
                }
            }
        }
    }
    if !added.is_empty() {
        let resources = input.resources;
        input.origins.record(
            added,
            |o| {
                let dir = resources.dir(o.kind);
                dir.join(&o.file).exists()
                    || dir.join(format!("{}{DISABLED_SUFFIX}", o.file)).exists()
            },
            &[],
        )?;
    }
    Ok(report)
}

/// Whether `selector` names `file`: a file name (with or without `.disabled`), a project id, a
/// mod id or a mod name.
fn matches(file: &InstalledFile, selector: &str) -> bool {
    let wanted = selector.trim();
    let bare = wanted.strip_suffix(DISABLED_SUFFIX).unwrap_or(wanted);
    file.file.eq_ignore_ascii_case(bare)
        || file.name.eq_ignore_ascii_case(wanted)
        || file
            .identity
            .as_ref()
            .is_some_and(|i| i.project_id == wanted)
        || file
            .mod_ids
            .iter()
            .any(|id| normalize_id(id) == normalize_id(wanted))
}

/// The checks of several platforms, merged so that every file is reported once.
struct Combined {
    report: UpdateReport,
    /// Each platform's inspection, in the order asked; `None` for one that was skipped.
    inspections: Vec<Option<Inspection>>,
    /// The platform (index) that takes each file.
    owners: HashMap<FileKey, usize>,
}

/// Errors that skip one platform of a combined check instead of failing it: no key, a refused
/// key, no network.
fn skippable(error: &Error) -> bool {
    matches!(
        error.kind,
        ErrorKind::NoApiKey | ErrorKind::Auth | ErrorKind::Network
    )
}

fn platform_name(provider: Provider) -> &'static str {
    match provider {
        Provider::CurseForge => "CurseForge",
        _ => "Modrinth",
    }
}

fn inspect_all(inputs: &[UpdateInput<'_>], options: UpdateOptions) -> Result<Combined> {
    let mut inspections = Vec::new();
    let mut skipped = Vec::new();
    let mut first_error = None;
    for input in inputs {
        match inspect(input, options) {
            Ok(inspection) => inspections.push(Some(inspection)),
            Err(e) if skippable(&e) => {
                skipped.push(format!(
                    "{} was not checked: {}",
                    platform_name(input.source.provider()),
                    e.message
                ));
                first_error.get_or_insert(e);
                inspections.push(None);
            }
            Err(e) => return Err(e),
        }
    }
    let Some(first) = inspections.iter().flatten().next() else {
        return Err(first_error.unwrap_or_else(|| Error::invalid_input("no platform to check")));
    };
    let mut report = UpdateReport {
        game_version: first.report.game_version.clone(),
        loader: first.report.loader.clone(),
        ..UpdateReport::default()
    };
    for warning in inspections
        .iter()
        .flatten()
        .flat_map(|i| &i.report.warnings)
        .chain(&skipped)
    {
        if !report.warnings.contains(warning) {
            report.warnings.push(warning.clone());
        }
    }

    // Every file once, in the order the platforms list them.
    let mut order: Vec<&FileKey> = Vec::new();
    let mut said: HashMap<&FileKey, Vec<(usize, Outcome)>> = HashMap::new();
    for (index, inspection) in inspections.iter().enumerate() {
        for (key, outcome) in inspection.iter().flat_map(|i| &i.outcomes) {
            let seen = said.entry(key).or_default();
            if seen.is_empty() {
                order.push(key);
            }
            seen.push((index, *outcome));
        }
    }
    let mut owners = HashMap::new();
    for key in order {
        let outcomes = said.get(key).map_or(&[][..], Vec::as_slice);
        let (kind, file) = (key.0, key.1.clone());
        let Some(&(owner, outcome)) = outcomes.iter().find(|(_, o)| o.taken()) else {
            // Nobody takes it: recorded as a platform's that was not asked, or unknown.
            let reason = if outcomes
                .iter()
                .any(|(_, o)| *o == Outcome::Unchecked(UncheckedReason::OtherPlatform))
            {
                UncheckedReason::OtherPlatform
            } else {
                UncheckedReason::NoOrigin
            };
            report.unchecked.push(UncheckedFile { kind, file, reason });
            continue;
        };
        owners.insert(key.clone(), owner);
        match outcome {
            Outcome::Update => {
                let candidate = inspections
                    .get(owner)
                    .and_then(Option::as_ref)
                    .and_then(|i| {
                        i.report
                            .updates
                            .iter()
                            .find(|c| c.kind == kind && c.file == file)
                    });
                if let Some(candidate) = candidate {
                    report.updates.push(candidate.clone());
                }
            }
            Outcome::UpToDate => report.up_to_date += 1,
            Outcome::FromPack => report.from_pack.push(PackFile { kind, file }),
            Outcome::Unchecked(reason) => {
                if reason == UncheckedReason::SameProject
                    && let Some(Some(inspection)) = inspections.get(owner)
                    && let Some(conflict) = conflict_of(&inspection.report.conflicts, kind, &file)
                    && !report.conflicts.contains(conflict)
                {
                    report.conflicts.push(conflict.clone());
                }
                report.unchecked.push(UncheckedFile { kind, file, reason });
            }
        }
    }
    Ok(Combined {
        report,
        inspections,
        owners,
    })
}

/// [`check`] on several platforms at once, in order of preference (Modrinth first). A file
/// with an origin record goes to the platform of its record; a file without one goes to the
/// first platform that knows it. Every file is reported once, so no update is offered twice. A
/// platform that cannot be asked (no key, a refused key, no network) is skipped with a warning
/// while another one answers.
///
/// # Errors
///
/// The error of the first skipped platform when none could be asked; `Cancelled`, `Io`.
pub fn check_all(inputs: &[UpdateInput<'_>], options: UpdateOptions) -> Result<UpdateReport> {
    Ok(inspect_all(inputs, options)?.report)
}

/// The files of `inspection` that `selector` names; a file name wins over a looser match.
fn selected<'i>(
    inspection: &'i Inspection,
    target: &Target,
    selector: &str,
) -> Vec<(ResourceKind, &'i InstalledFile)> {
    let mut hits: Vec<(ResourceKind, &InstalledFile)> = kinds(target)
        .iter()
        .flat_map(|k| {
            inspection
                .scanned
                .get(k)
                .into_iter()
                .flatten()
                .map(move |f| (*k, f))
        })
        .filter(|(_, f)| matches(f, selector))
        .collect();
    if hits.len() > 1 {
        let exact: Vec<_> = hits
            .iter()
            .filter(|(_, f)| f.file.eq_ignore_ascii_case(selector))
            .copied()
            .collect();
        if !exact.is_empty() {
            hits = exact;
        }
    }
    hits
}

/// What updating `selection` would do: the plan of the new versions with their new required
/// dependencies. With `version`, the one selected file is switched to that version instead
/// (older ones included).
///
/// # Errors
///
/// - `NotFound`: nothing matches a selector;
/// - `InvalidInput`: `version` without exactly one selected file;
/// - `Conflict`: a selected file came with the modpack and `include_pack_files` is off;
/// - `Unsupported`: the file's origin is unknown, or recorded as another platform's, so no
///   project can be named here;
/// - the errors of the planner and the network.
pub fn plan_update(
    input: &UpdateInput<'_>,
    options: UpdateOptions,
    selection: Selection<'_>,
    version: Option<&str>,
) -> Result<InstallPlan> {
    plan_update_all(std::slice::from_ref(input), options, selection, version)
}

/// [`plan_update`] on several platforms at once, with the files shared out as [`check_all`]
/// does: each file is updated from the platform that takes it, and the plans of the platforms
/// are put together into one.
///
/// # Errors
///
/// As [`plan_update`].
pub fn plan_update_all(
    inputs: &[UpdateInput<'_>],
    options: UpdateOptions,
    selection: Selection<'_>,
    version: Option<&str>,
) -> Result<InstallPlan> {
    if version.is_some() && !matches!(selection, Selection::Files(f) if f.len() == 1) {
        return Err(Error::invalid_input(
            "choosing a version needs exactly one mod",
        ));
    }
    let combined = inspect_all(inputs, options)?;
    let mut warnings = combined.report.warnings.clone();
    let mut pins: Vec<Vec<(String, String, ResourceKind)>> = vec![Vec::new(); inputs.len()];
    let mut pin = |owner: usize, project: &str, version: &str, kind: ResourceKind| {
        if let Some(list) = pins.get_mut(owner)
            && !list.iter().any(|(p, _, _)| p == project)
        {
            list.push((project.to_owned(), version.to_owned(), kind));
        }
    };
    let owner_of =
        |kind: ResourceKind, file: &str| combined.owners.get(&(kind, file.to_owned())).copied();

    match selection {
        Selection::All => {
            for c in &combined.report.updates {
                if let Some(owner) = owner_of(c.kind, &c.file) {
                    pin(owner, &c.project_id, &c.latest.id, c.kind);
                }
            }
            for c in &combined.report.conflicts {
                warnings.push(format!(
                    "{} are files of the same project; none of them is updated until one is \
                     removed",
                    c.files.join(", ")
                ));
            }
        }
        Selection::Files(selectors) => {
            for selector in selectors {
                let mut hits: Vec<(usize, ResourceKind, &InstalledFile)> = Vec::new();
                let mut unowned: Option<&InstalledFile> = None;
                for (index, inspection) in combined.inspections.iter().enumerate() {
                    let (Some(inspection), Some(input)) = (inspection, inputs.get(index)) else {
                        continue;
                    };
                    for (kind, file) in selected(inspection, input.target, selector) {
                        match owner_of(kind, &file.file) {
                            Some(owner) if owner == index => hits.push((index, kind, file)),
                            Some(_) => {}
                            None => {
                                unowned.get_or_insert(file);
                            }
                        }
                    }
                }
                if hits.is_empty() {
                    return Err(match unowned {
                        Some(file) => match file.foreign {
                            Some(provider) => Error::unsupported(format!(
                                "{} was installed from {}; update it there",
                                file.file,
                                platform_name(provider)
                            )),
                            None => Error::unsupported(format!(
                                "{} has no known origin, so its project is unknown",
                                file.file
                            )),
                        },
                        None => {
                            Error::not_found(format!("no mod, pack or shader matches {selector}"))
                        }
                    });
                }
                for (owner, kind, file) in hits {
                    let Some(Some(inspection)) = combined.inspections.get(owner) else {
                        continue;
                    };
                    if inspection.pack_files.contains(&(kind, file.file.clone()))
                        && !options.include_pack_files
                    {
                        return Err(Error::new(
                            ErrorKind::Conflict,
                            format!(
                                "{} came with the modpack; update the pack, or include its files \
                                 explicitly",
                                file.file
                            ),
                        ));
                    }
                    if let Some(c) = conflict_of(&inspection.report.conflicts, kind, &file.file) {
                        return Err(Error::new(
                            ErrorKind::Conflict,
                            format!(
                                "{} are files of the same project; remove all but one first",
                                c.files.join(", ")
                            ),
                        ));
                    }
                    let Some(identity) = &file.identity else {
                        return Err(Error::unsupported(format!(
                            "{} has no known origin, so its project is unknown",
                            file.file
                        )));
                    };
                    if let Some(wanted) = version {
                        if wanted == identity.version_id {
                            warnings.push(format!("{} is already that version", file.file));
                        } else {
                            pin(owner, &identity.project_id, wanted, kind);
                        }
                    } else if let Some(c) = inspection
                        .report
                        .updates
                        .iter()
                        .find(|c| c.kind == kind && c.file == file.file)
                    {
                        pin(owner, &c.project_id, &c.latest.id, kind);
                    } else {
                        warnings.push(format!("{} has no newer version", file.file));
                    }
                }
            }
        }
    }

    // One plan per platform with something to do; with nothing to do, the first platform
    // that answered gives the empty plan.
    let mut planned: Vec<usize> = (0..inputs.len())
        .filter(|&i| pins.get(i).is_some_and(|p| !p.is_empty()))
        .collect();
    if planned.is_empty()
        && let Some(first) = combined.inspections.iter().position(Option::is_some)
    {
        planned.push(first);
    }
    let mut merged: Option<InstallPlan> = None;
    for index in planned {
        let (Some(input), Some(Some(inspection)), Some(pins)) = (
            inputs.get(index),
            combined.inspections.get(index),
            pins.get(index),
        ) else {
            continue;
        };
        let plan_input = PlanInput {
            source: input.source,
            resources: input.resources,
            origins: input.origins,
            target: input.target,
            control: input.control,
        };
        let mut planner = Planner::new(&plan_input, options.min);
        planner.seed(inspection.scanned.clone(), inspection.latest.clone());
        planner.add_pinned(pins)?;
        let plan = planner.finish();
        merged = Some(match merged {
            None => plan,
            Some(mut all) => {
                for item in plan.items {
                    // A file both platforms plan (a shared dependency) is taken once.
                    if !all
                        .items
                        .iter()
                        .any(|i| i.kind == item.kind && i.file.filename == item.file.filename)
                    {
                        all.items.push(item);
                    }
                }
                all.satisfied.extend(plan.satisfied);
                all.recommended.extend(plan.recommended);
                all.warnings.extend(plan.warnings);
                all
            }
        });
    }
    let mut plan = merged.ok_or_else(|| Error::internal("no platform was planned"))?;
    warnings.append(&mut plan.warnings);
    plan.warnings = warnings;
    Ok(plan)
}
