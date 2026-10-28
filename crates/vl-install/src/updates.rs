//! Origins of hand-added files, update checks and updates.
//!
//! [`identify`] finds, by hash, which catalog project a file in a resource folder belongs to
//! and records it like an install would. [`check`] looks for newer versions of every mod, resource
//! pack, shader pack and data pack of an instance: one batched lookup of all file hashes, then one
//! batched "newest version for these files" request for the mods (filtered by game version,
//! loader and release channel) and one for the other kinds. Only a version that is newer than the
//! installed one is offered, so an update never goes back to an older version.
//!
//! Files that came with the instance's modpack are left out unless asked for: the pack is
//! updated as a whole. [`plan_update`] turns a selection into an [`InstallPlan`] through the same
//! planner as an install, so new required dependencies are resolved the same way; the plan is
//! carried out by [`crate::execute`], which replaces files all together or not at all and keeps
//! disabled files disabled. Switching a file to a chosen version, older ones included, is the
//! same plan with an explicit version.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use vl_core::catalog::{HashAlgorithm, ProjectVersion, ReleaseChannel, VersionFilter};
use vl_core::install::{
    IdentifiedFile, IdentifyReport, InstallPlan, MatchedBy, MinStability, PackFile, UncheckedFile,
    UncheckedReason, UpdateCandidate, UpdateReport, VersionLabel,
};
use vl_core::resources::ResourceKind;
use vl_core::{Error, ErrorKind, Result};
use vl_net::Control;
use vl_platforms::CatalogSource;
use vl_resources::{DISABLED_SUFFIX, Resources};

use crate::mrpack::PackRecord;
use crate::origin::{Origin, OriginStore, now_unix};
use crate::plan::{PlanInput, Planner, Target};
use crate::scan::{InstalledFile, normalize_id, scan_many};

/// The folders that are checked.
const KINDS: [ResourceKind; 4] = [
    ResourceKind::Mods,
    ResourceKind::ResourcePacks,
    ResourceKind::ShaderPacks,
    ResourceKind::DataPacks,
];

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
    /// Also handle files that came with the modpack.
    pub include_pack_files: bool,
}

impl Default for UpdateOptions {
    fn default() -> Self {
        Self {
            min: MinStability::Release,
            include_pack_files: false,
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
/// it is checked to be newer, to fit and to be of an allowed channel.
fn is_update(
    current: &ProjectVersion,
    new: &ProjectVersion,
    target: &Target,
    loaders: &[String],
    min: MinStability,
) -> bool {
    new.id != current.id
        && new.project_id == current.project_id
        && new.date_published > current.date_published
        && new.game_versions.contains(&target.game_version)
        && (loaders.is_empty() || new.loaders.iter().any(|l| loaders.contains(l)))
        && min.rank(new.channel).is_some()
}

/// A file to ask the platform about.
struct Asked<'a> {
    kind: ResourceKind,
    file: &'a InstalledFile,
    current: &'a ProjectVersion,
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
        &KINDS,
        &loaded.origins,
        input.source,
        input.control,
    )?;
    let pack = pack_hashes(input.meta_dir);
    let mut pack_files = HashSet::new();

    let mut mods: Vec<Asked<'_>> = Vec::new();
    let mut others: Vec<Asked<'_>> = Vec::new();
    for kind in KINDS {
        for file in scanned.get(&kind).map_or(&[][..], Vec::as_slice) {
            if pack.contains(&file.hashes.sha1) || pack.contains(&file.hashes.sha512) {
                pack_files.insert((kind, file.file.clone()));
                if !options.include_pack_files {
                    report.from_pack.push(PackFile {
                        kind,
                        file: file.file.clone(),
                    });
                    continue;
                }
            }
            let unchecked = |reason| UncheckedFile {
                kind,
                file: file.file.clone(),
                reason,
            };
            let Some(identity) = file.identity.as_ref() else {
                report.unchecked.push(unchecked(UncheckedReason::NoOrigin));
                continue;
            };
            let Some(current) = identity.version.as_ref() else {
                report
                    .unchecked
                    .push(unchecked(UncheckedReason::UnknownToPlatform));
                continue;
            };
            if kind == ResourceKind::Mods {
                if input.target.loader.is_none() {
                    report.unchecked.push(unchecked(UncheckedReason::NoLoader));
                } else {
                    mods.push(Asked {
                        kind,
                        file,
                        current,
                    });
                }
            } else {
                others.push(Asked {
                    kind,
                    file,
                    current,
                });
            }
        }
    }

    let channels = channels_for(options.min);
    let mut latest: HashMap<String, ProjectVersion> = HashMap::new();
    let mut found: Vec<(&Asked<'_>, ProjectVersion)> = Vec::new();
    let mut up_to_date = 0u32;

    let mod_loaders = match input.target.loader.as_deref() {
        Some("quilt") => vec!["quilt".to_owned(), "fabric".to_owned()],
        Some(other) => vec![other.to_owned()],
        None => Vec::new(),
    };
    // Packs and shaders name their own loaders (`minecraft`, `iris`, ...): the request allows
    // the ones the installed files have.
    let mut other_loaders: Vec<String> = others
        .iter()
        .flat_map(|a| a.current.loaders.iter().cloned())
        .collect();
    other_loaders.sort();
    other_loaders.dedup();

    for (group, loaders) in [(&mods, mod_loaders), (&others, other_loaders)] {
        if group.is_empty() {
            continue;
        }
        let hashes: Vec<String> = group.iter().map(|a| a.file.hashes.sha1.clone()).collect();
        let filter = VersionFilter {
            loaders: loaders.clone(),
            game_versions: vec![input.target.game_version.clone()],
        };
        let answers: HashMap<String, ProjectVersion> = input
            .source
            .latest_versions_by_hashes(
                HashAlgorithm::Sha1,
                &hashes,
                &filter,
                &channels,
                input.control,
            )?
            .into_iter()
            .map(|m| (m.hash.to_ascii_lowercase(), m.version))
            .collect();
        for asked in group {
            match answers.get(&asked.file.hashes.sha1) {
                Some(new) if is_update(asked.current, new, input.target, &loaders, options.min) => {
                    latest.insert(new.id.clone(), new.clone());
                    found.push((asked, new.clone()));
                }
                _ => up_to_date += 1,
            }
        }
    }

    for (asked, new) in found {
        report.updates.push(UpdateCandidate {
            kind: asked.kind,
            file: asked.file.file.clone(),
            enabled: asked.file.enabled,
            name: asked.file.name.clone(),
            project_id: asked.current.project_id.clone(),
            current: label(asked.current),
            latest: label(&new),
            from_pack: pack_files.contains(&(asked.kind, asked.file.file.clone())),
        });
    }
    report.up_to_date = up_to_date;
    Ok(Inspection {
        report,
        scanned,
        latest,
        pack_files,
    })
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
        &KINDS,
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
    for kind in KINDS {
        for file in scanned.get(&kind).map_or(&[][..], Vec::as_slice) {
            match &file.identity {
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

/// What updating `selection` would do: the plan of the new versions with their new required
/// dependencies. With `version`, the one selected file is switched to that version instead
/// (older ones included).
///
/// # Errors
///
/// - `NotFound`: nothing matches a selector;
/// - `InvalidInput`: `version` without exactly one selected file;
/// - `Conflict`: a selected file came with the modpack and `include_pack_files` is off;
/// - `Unsupported`: the file's origin is unknown, so no project can be named;
/// - the errors of the planner and the network.
pub fn plan_update(
    input: &UpdateInput<'_>,
    options: UpdateOptions,
    selection: Selection<'_>,
    version: Option<&str>,
) -> Result<InstallPlan> {
    if version.is_some() && !matches!(selection, Selection::Files(f) if f.len() == 1) {
        return Err(Error::invalid_input(
            "choosing a version needs exactly one mod",
        ));
    }
    let inspection = inspect(input, options)?;
    let mut warnings = inspection.report.warnings.clone();
    let mut pins: Vec<(String, String)> = Vec::new();
    let mut pin = |project: &str, version: &str| {
        if !pins.iter().any(|(p, _)| p == project) {
            pins.push((project.to_owned(), version.to_owned()));
        }
    };

    match selection {
        Selection::All => {
            for c in &inspection.report.updates {
                pin(&c.project_id, &c.latest.id);
            }
        }
        Selection::Files(selectors) => {
            for selector in selectors {
                let mut hits: Vec<(ResourceKind, &InstalledFile)> = KINDS
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
                // A file name wins over a looser match.
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
                if hits.is_empty() {
                    return Err(Error::not_found(format!(
                        "no mod, pack or shader matches {selector}"
                    )));
                }
                for (kind, file) in hits {
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
                            pin(&identity.project_id, wanted);
                        }
                    } else if let Some(c) = inspection
                        .report
                        .updates
                        .iter()
                        .find(|c| c.kind == kind && c.file == file.file)
                    {
                        pin(&c.project_id, &c.latest.id);
                    } else {
                        warnings.push(format!("{} has no newer version", file.file));
                    }
                }
            }
        }
    }

    let plan_input = PlanInput {
        source: input.source,
        resources: input.resources,
        origins: input.origins,
        target: input.target,
        control: input.control,
    };
    let mut planner = Planner::new(&plan_input, options.min);
    planner.seed(inspection.scanned.clone(), inspection.latest.clone());
    planner.add_pinned(&pins)?;
    let mut plan = planner.finish();
    warnings.extend(plan.warnings);
    plan.warnings = warnings;
    Ok(plan)
}
