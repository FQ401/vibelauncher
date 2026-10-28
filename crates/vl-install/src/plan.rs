//! Working out what installing a project needs: the version, its required dependencies, what is
//! already in the folders and what would clash.

use std::collections::{HashMap, HashSet};

use vl_core::catalog::{
    DependencyKind, Project, ProjectType, ProjectVersion, ReleaseChannel, VersionFile,
    VersionFilter,
};
use vl_core::install::{
    InstallPlan, MatchedBy, MinStability, PlanAction, PlanItem, PlanRole, Recommendation, Satisfied,
};
use vl_core::resources::ResourceKind;
use vl_core::{Error, ErrorKind, Result};
use vl_net::Control;
use vl_platforms::CatalogSource;
use vl_resources::Resources;

use crate::origin::{Origin, OriginStore};
use crate::scan::{InstalledFile, normalize_id, scan};

/// The most dependency levels followed.
const MAX_DEPTH: usize = 16;
/// The most projects in one plan.
const MAX_PROJECTS: usize = 64;
/// The most optional dependencies whose names are looked up.
const MAX_RECOMMENDED: usize = 12;

/// The instance an install goes into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The game version.
    pub game_version: String,
    /// The mod loader (`fabric`, `quilt`, `forge`, `neoforge`), when the instance has one.
    pub loader: Option<String>,
}

impl Target {
    /// The loaders whose mods run here, the instance's own first. Quilt runs Fabric mods.
    fn loaders(&self) -> Vec<String> {
        match self.loader.as_deref() {
            None => Vec::new(),
            Some("quilt") => vec!["quilt".into(), "fabric".into()],
            Some(other) => vec![other.to_owned()],
        }
    }
}

/// What a plan is built from.
pub struct PlanInput<'a> {
    /// The platform.
    pub source: &'a dyn CatalogSource,
    /// The instance's resource folders.
    pub resources: &'a Resources,
    /// The origin records of the instance.
    pub origins: &'a OriginStore,
    /// The instance's game version and loader.
    pub target: &'a Target,
    /// Cancellation.
    pub control: &'a Control,
}

/// The folder a project of `project_type` is installed into.
pub fn resource_kind(project_type: ProjectType) -> Option<ResourceKind> {
    match project_type {
        ProjectType::Mod => Some(ResourceKind::Mods),
        ProjectType::ResourcePack => Some(ResourceKind::ResourcePacks),
        ProjectType::Shader => Some(ResourceKind::ShaderPacks),
        ProjectType::DataPack => Some(ResourceKind::DataPacks),
        _ => None,
    }
}

/// The extension a file of `kind` must have (the rule of `Resources::add`).
fn extension(kind: ResourceKind) -> &'static str {
    if kind == ResourceKind::Mods {
        "jar"
    } else {
        "zip"
    }
}

/// Builds the plan for `project` (id or slug).
///
/// `version` installs that exact version instead of choosing one; `min` limits the channels
/// considered when choosing (the most stable available wins, then the newest).
///
/// # Errors
///
/// - `NotFound`: no project, no version for this game version and loader, or no allowed channel;
/// - `Unsupported`: the project is a modpack;
/// - `Conflict`: an incompatible mod is installed or required, or a different file has the name;
/// - `Integrity`: the platform gave no usable hash or file;
/// - the errors of the network and the folders.
pub fn plan(
    input: &PlanInput<'_>,
    project: &str,
    version: Option<&str>,
    min: MinStability,
) -> Result<InstallPlan> {
    let root = input.source.project(project, input.control)?;
    let mut planner = Planner::new(input, min);
    planner.add(root, version, PlanRole::Requested, 0)?;
    Ok(planner.finish())
}

struct Planner<'a> {
    input: &'a PlanInput<'a>,
    min: MinStability,
    origins: Vec<Origin>,
    scanned: HashMap<ResourceKind, Vec<InstalledFile>>,
    seen: HashSet<String>,
    /// Project id → title of the project whose chosen version declares it incompatible.
    incompatible: HashMap<String, String>,
    items: Vec<PlanItem>,
    satisfied: Vec<Satisfied>,
    optional: Vec<(String, String)>,
    warnings: Vec<String>,
}

impl<'a> Planner<'a> {
    fn new(input: &'a PlanInput<'a>, min: MinStability) -> Self {
        let loaded = input.origins.load();
        let mut warnings = Vec::new();
        if loaded.damaged {
            warnings.push(format!(
                "the origin records {} are damaged and were partly ignored",
                input.origins.path().display()
            ));
        }
        Self {
            input,
            min,
            origins: loaded.origins,
            scanned: HashMap::new(),
            seen: HashSet::new(),
            incompatible: HashMap::new(),
            items: Vec::new(),
            satisfied: Vec::new(),
            optional: Vec::new(),
            warnings,
        }
    }

    fn installed(&mut self, kind: ResourceKind) -> Result<&[InstalledFile]> {
        if !self.scanned.contains_key(&kind) {
            let files = scan(
                self.input.resources,
                kind,
                &self.origins,
                self.input.source,
                self.input.control,
            )?;
            self.scanned.insert(kind, files);
        }
        Ok(self.scanned.get(&kind).map_or(&[], Vec::as_slice))
    }

    fn add(
        &mut self,
        project: Project,
        pinned: Option<&str>,
        role: PlanRole,
        depth: usize,
    ) -> Result<()> {
        if !self.seen.insert(project.id.clone()) {
            return Ok(());
        }
        if depth > MAX_DEPTH || self.seen.len() > MAX_PROJECTS {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!(
                    "the dependencies of {} are nested too deeply or number too many",
                    project.title
                ),
            ));
        }
        let kind = resource_kind(project.project_type).ok_or_else(|| {
            Error::unsupported(format!(
                "{} is a {} project, which cannot be installed into an instance folder",
                project.title,
                project.project_type.name()
            ))
        })?;
        let is_dependency = matches!(role, PlanRole::Dependency { .. });

        let hit = self
            .installed(kind)?
            .iter()
            .find(|f| {
                f.identity
                    .as_ref()
                    .is_some_and(|i| i.project_id == project.id)
            })
            .cloned();
        if is_dependency && pinned.is_none() {
            let by_id = match hit.clone() {
                Some(h) => Some((h, None)),
                None => self
                    .by_mod_id(kind, &project)
                    .map(|f| (f, Some(MatchedBy::ModId))),
            };
            if let Some((file, guess)) = by_id {
                let identity = file.identity.as_ref();
                self.satisfied.push(Satisfied {
                    project_id: project.id.clone(),
                    slug: project.slug.clone(),
                    title: project.title.clone(),
                    file: file.file.clone(),
                    version: identity.and_then(|i| i.version_number.clone()),
                    matched_by: guess
                        .or_else(|| identity.map(|i| i.matched_by))
                        .unwrap_or(MatchedBy::ModId),
                });
                return Ok(());
            }
        }

        let version = self.choose(&project, kind, pinned)?;
        let file = pick_file(&project, &version, kind)?;
        self.check_incompatible(&project, &version, kind)?;

        let action = self.action(&project, &version, &file, kind, hit.as_ref())?;
        let required_by = project.title.clone();
        for dependency in &version.dependencies {
            match (dependency.kind, dependency.project_id.as_deref()) {
                (DependencyKind::Required, Some(id)) => {
                    let wanted = self.input.source.project(id, self.input.control)?;
                    if resource_kind(wanted.project_type).is_none() {
                        self.warnings.push(format!(
                            "{required_by} needs {}, a {} project that is not installed",
                            wanted.title,
                            wanted.project_type.name()
                        ));
                        continue;
                    }
                    let role = PlanRole::Dependency {
                        required_by: required_by.clone(),
                    };
                    self.add(wanted, dependency.version_id.as_deref(), role, depth + 1)?;
                }
                (DependencyKind::Required, None) => {
                    let what = dependency
                        .file_name
                        .as_deref()
                        .or(dependency.version_id.as_deref())
                        .unwrap_or("a file");
                    self.warnings.push(format!(
                        "{required_by} needs {what}, which the platform does not name as a project; \
                         install it by hand"
                    ));
                }
                (DependencyKind::Optional, Some(id)) => {
                    self.optional.push((id.to_owned(), required_by.clone()));
                }
                _ => {}
            }
        }

        self.items.push(PlanItem {
            provider: self.input.source.provider(),
            project_id: project.id,
            slug: project.slug,
            title: project.title,
            project_type: project.project_type,
            kind,
            role,
            version_id: version.id,
            version_number: version.version_number,
            channel: version.channel,
            file,
            action,
        });
        Ok(())
    }

    /// A file whose declared mod id is the project's slug.
    fn by_mod_id(&mut self, kind: ResourceKind, project: &Project) -> Option<InstalledFile> {
        if kind != ResourceKind::Mods {
            return None;
        }
        let slug = normalize_id(&project.slug);
        let files = self.installed(kind).ok()?;
        files
            .iter()
            .find(|f| f.mod_ids.iter().any(|id| normalize_id(id) == slug))
            .cloned()
    }

    /// The version to install: the pinned one, or the best allowed for the instance.
    fn choose(
        &self,
        project: &Project,
        kind: ResourceKind,
        pinned: Option<&str>,
    ) -> Result<ProjectVersion> {
        let target = self.input.target;
        let loaders = if kind == ResourceKind::Mods {
            if target.loader.is_none() {
                return Err(Error::invalid_input(
                    "the instance has no mod loader; add Fabric, Quilt, Forge or NeoForge first",
                ));
            }
            target.loaders()
        } else {
            Vec::new()
        };
        let filter = VersionFilter {
            loaders: loaders.clone(),
            game_versions: vec![target.game_version.clone()],
        };
        let listed = self
            .input
            .source
            .versions(&project.id, &filter, self.input.control)?;
        // The platform filters too, but its answer is data: check it.
        let fits: Vec<ProjectVersion> = listed
            .into_iter()
            .filter(|v| v.game_versions.contains(&target.game_version))
            .filter(|v| loaders.is_empty() || v.loaders.iter().any(|l| loaders.contains(l)))
            .collect();
        let what = || {
            let loader = target
                .loader
                .as_deref()
                .filter(|_| kind == ResourceKind::Mods)
                .map(|l| format!(" on {l}"))
                .unwrap_or_default();
            format!(
                "{} for Minecraft {}{loader}",
                project.title, target.game_version
            )
        };
        if let Some(id) = pinned {
            return fits.into_iter().find(|v| v.id == id).ok_or_else(|| {
                Error::not_found(format!(
                    "version {id} of {} does not exist or does not fit",
                    what()
                ))
            });
        }
        if fits.is_empty() {
            return Err(Error::not_found(format!("no version of {}", what())));
        }
        let min = self.min;
        let primary = loaders.first().cloned();
        let best = fits
            .iter()
            .filter_map(|v| {
                let rank = min.rank(v.channel)?;
                let native = primary.as_ref().is_some_and(|l| v.loaders.contains(l));
                Some(((native, rank, v.date_published.as_str()), v))
            })
            .max_by(|a, b| a.0.cmp(&b.0))
            .map(|(_, v)| v.clone());
        best.ok_or_else(|| {
            let channels = |c: ReleaseChannel| fits.iter().filter(|v| v.channel == c).count();
            Error::not_found(format!(
                "no {} version of {} ({} beta, {} alpha exist; allow them with a less strict \
                 channel)",
                min.name(),
                what(),
                channels(ReleaseChannel::Beta),
                channels(ReleaseChannel::Alpha)
            ))
        })
    }

    /// Fails when the chosen version clashes with something installed or already planned.
    fn check_incompatible(
        &mut self,
        project: &Project,
        version: &ProjectVersion,
        kind: ResourceKind,
    ) -> Result<()> {
        let clash = |what: String| Error::new(ErrorKind::Conflict, what);
        if let Some(by) = self.incompatible.get(&project.id) {
            return Err(clash(format!(
                "{} is incompatible with {by}, which is part of this install",
                project.title
            )));
        }
        let declared: Vec<&str> = version
            .dependencies
            .iter()
            .filter(|d| d.kind == DependencyKind::Incompatible)
            .filter_map(|d| d.project_id.as_deref())
            .collect();
        for id in &declared {
            if self.seen.contains(*id) && *id != project.id {
                return Err(clash(format!(
                    "{} {} is incompatible with a project that is part of this install ({id})",
                    project.title, version.version_number
                )));
            }
        }
        let own_id = project.id.clone();
        let installed = self.installed(kind)?;
        for file in installed {
            let Some(identity) = &file.identity else {
                continue;
            };
            if identity.project_id == own_id {
                continue;
            }
            if declared.contains(&identity.project_id.as_str()) {
                return Err(clash(format!(
                    "{} {} is incompatible with the installed {} ({}); remove it first",
                    project.title, version.version_number, identity.project_id, file.file
                )));
            }
            let declares_us = identity.version.as_ref().is_some_and(|v| {
                v.dependencies.iter().any(|d| {
                    d.kind == DependencyKind::Incompatible
                        && d.project_id.as_deref() == Some(own_id.as_str())
                })
            });
            if declares_us {
                return Err(clash(format!(
                    "the installed {} declares itself incompatible with {}; remove it first",
                    file.file, project.title
                )));
            }
        }
        for id in declared {
            self.incompatible
                .insert(id.to_owned(), project.title.clone());
        }
        Ok(())
    }

    /// What happens to the file of `version` given what is in the folder.
    fn action(
        &mut self,
        project: &Project,
        version: &ProjectVersion,
        file: &VersionFile,
        kind: ResourceKind,
        hit: Option<&InstalledFile>,
    ) -> Result<PlanAction> {
        let installed = self.installed(kind)?;
        let same_content = |f: &InstalledFile| {
            file.sha1
                .as_deref()
                .is_some_and(|h| h.eq_ignore_ascii_case(&f.hashes.sha1))
                || file
                    .sha512
                    .as_deref()
                    .is_some_and(|h| h.eq_ignore_ascii_case(&f.hashes.sha512))
        };
        if let Some(present) = installed.iter().find(|f| same_content(f)) {
            return Ok(PlanAction::Present {
                file: present.file.clone(),
            });
        }
        let taken = |name: &str, except: Option<&str>| {
            installed
                .iter()
                .any(|f| f.file.eq_ignore_ascii_case(name) && Some(f.file.as_str()) != except)
        };
        match hit {
            Some(old)
                if old
                    .identity
                    .as_ref()
                    .is_some_and(|i| i.version_id == version.id) =>
            {
                Ok(PlanAction::Present {
                    file: old.file.clone(),
                })
            }
            Some(old) => {
                if taken(&file.filename, Some(&old.file)) {
                    return Err(name_clash(&file.filename, project));
                }
                Ok(PlanAction::Replace {
                    file: old.file.clone(),
                    old_version: old.identity.as_ref().and_then(|i| i.version_number.clone()),
                })
            }
            None => {
                if taken(&file.filename, None) {
                    return Err(name_clash(&file.filename, project));
                }
                Ok(PlanAction::Install)
            }
        }
    }

    fn finish(mut self) -> InstallPlan {
        let recommended = self.recommendations();
        InstallPlan {
            game_version: self.input.target.game_version.clone(),
            loader: self.input.target.loader.clone(),
            items: self.items,
            satisfied: self.satisfied,
            recommended,
            warnings: self.warnings,
        }
    }

    /// Optional dependencies that are neither planned nor installed, named when the platform
    /// answers.
    fn recommendations(&mut self) -> Vec<Recommendation> {
        let mut out: Vec<Recommendation> = Vec::new();
        let candidates = std::mem::take(&mut self.optional);
        let planned: HashSet<String> = self.seen.clone();
        let installed_ids: HashSet<String> = self
            .scanned
            .values()
            .flatten()
            .filter_map(|f| f.identity.as_ref().map(|i| i.project_id.clone()))
            .collect();
        for (id, by) in candidates {
            if planned.contains(&id)
                || installed_ids.contains(&id)
                || out.iter().any(|r| r.project_id == id)
            {
                continue;
            }
            if out.len() >= MAX_RECOMMENDED {
                break;
            }
            let named = self.input.source.project(&id, self.input.control).ok();
            // An optional dependency installed without being recognized by hash.
            if let Some(p) = &named
                && let Some(kind) = resource_kind(p.project_type)
                && self.by_mod_id(kind, p).is_some()
            {
                continue;
            }
            out.push(Recommendation {
                project_id: id,
                slug: named.as_ref().map(|p| p.slug.clone()),
                title: named.map(|p| p.title),
                suggested_by: by,
            });
        }
        out
    }
}

fn name_clash(name: &str, project: &Project) -> Error {
    Error::new(
        ErrorKind::Conflict,
        format!(
            "a different file named {name} is already in the folder, so {} cannot be installed; \
             rename or remove that file",
            project.title
        ),
    )
}

/// The file to download for `version`, with the checks that make it safe to fetch.
fn pick_file(
    project: &Project,
    version: &ProjectVersion,
    kind: ResourceKind,
) -> Result<VersionFile> {
    let file = version.primary_file().cloned().ok_or_else(|| {
        Error::new(
            ErrorKind::Integrity,
            format!(
                "{} {} has no file to download",
                project.title, version.version_number
            ),
        )
    })?;
    let hashed = file.sha512.is_some() || file.sha1.is_some();
    if !hashed {
        return Err(Error::new(
            ErrorKind::Integrity,
            format!(
                "{} {} has no hash to check the download against",
                project.title, version.version_number
            ),
        ));
    }
    let wanted = extension(kind);
    let ok = file
        .filename
        .rsplit_once('.')
        .is_some_and(|(_, e)| e.eq_ignore_ascii_case(wanted));
    if !ok {
        return Err(Error::new(
            ErrorKind::Unsupported,
            format!(
                "{} is not a .{wanted} file, which {} needs",
                file.filename,
                kind.name()
            ),
        ));
    }
    Ok(file)
}
