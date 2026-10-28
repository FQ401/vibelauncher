//! Planning and installing against a synthetic catalog, a scripted transport and synthetic jars.

use std::collections::HashMap;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use sha1::Sha1;
use sha2::{Digest, Sha512};
use vl_core::catalog::{
    Dependency, DependencyKind, HashAlgorithm, HashMatch, Project, ProjectType, ProjectVersion,
    Provider, ReleaseChannel, SearchPage, SearchQuery, SideSupport, VersionFile, VersionFilter,
};
use vl_core::install::{InstallPlan, MatchedBy, MinStability, PlanAction, PlanRole};
use vl_core::resources::ResourceKind;
use vl_core::{Error, ErrorKind, Result};
use vl_net::{
    CancelToken, Client, Control, HttpRequest, HttpResponse, HttpTransport, NetConfig, NetError,
};
use vl_platforms::CatalogSource;
use vl_resources::Resources;

use crate::origin::{Origin, OriginStore};
use crate::{ApplyInput, PlanInput, Target, execute, plan};

struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vl-install-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha1(data: &[u8]) -> String {
    hex(&Sha1::digest(data))
}

fn sha512(data: &[u8]) -> String {
    hex(&Sha512::digest(data))
}

/// A jar with a `fabric.mod.json` that declares `id`.
fn jar(id: &str, version: &str) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file("fabric.mod.json", zip::write::SimpleFileOptions::default())
        .unwrap();
    let manifest = format!(r#"{{"schemaVersion":1,"id":"{id}","version":"{version}"}}"#);
    zip.write_all(manifest.as_bytes()).unwrap();
    zip.finish().unwrap().into_inner()
}

fn project(id: &str, kind: ProjectType) -> Project {
    Project {
        id: id.to_owned(),
        slug: id.to_lowercase(),
        title: id.to_owned(),
        description: String::new(),
        body: String::new(),
        project_type: kind,
        categories: Vec::new(),
        loaders: Vec::new(),
        game_versions: Vec::new(),
        downloads: 0,
        followers: 0,
        icon_url: None,
        license: None,
        source_url: None,
        issues_url: None,
        client_side: SideSupport::Unknown,
        server_side: SideSupport::Unknown,
        updated: String::new(),
        version_ids: Vec::new(),
    }
}

/// A version of a project with one file.
#[derive(Clone)]
struct Ver {
    id: String,
    project: String,
    number: String,
    channel: ReleaseChannel,
    loaders: Vec<String>,
    game: String,
    date: String,
    bytes: Vec<u8>,
    deps: Vec<Dependency>,
    filename: String,
}

fn ver(id: &str, project: &str, number: &str) -> Ver {
    Ver {
        id: id.to_owned(),
        project: project.to_owned(),
        number: number.to_owned(),
        channel: ReleaseChannel::Release,
        loaders: vec!["fabric".into()],
        game: "1.21.4".into(),
        date: "2025-01-01T00:00:00Z".into(),
        bytes: jar(&project.to_lowercase(), number),
        deps: Vec::new(),
        filename: format!("{}-{number}.jar", project.to_lowercase()),
    }
}

impl Ver {
    fn channel(mut self, c: ReleaseChannel) -> Self {
        self.channel = c;
        self
    }

    fn date(mut self, d: &str) -> Self {
        self.date = d.to_owned();
        self
    }

    fn loaders(mut self, l: &[&str]) -> Self {
        self.loaders = l.iter().map(|s| (*s).to_owned()).collect();
        self
    }

    fn game(mut self, g: &str) -> Self {
        self.game = g.to_owned();
        self
    }

    fn dep(mut self, project: &str, kind: DependencyKind) -> Self {
        self.deps.push(Dependency {
            project_id: Some(project.to_owned()),
            version_id: None,
            file_name: None,
            kind,
        });
        self
    }

    fn file_name(mut self, n: &str) -> Self {
        self.filename = n.to_owned();
        self
    }

    fn url(&self) -> String {
        format!("https://files.test/{}/{}", self.id, self.filename)
    }

    fn build(&self) -> ProjectVersion {
        ProjectVersion {
            id: self.id.clone(),
            project_id: self.project.clone(),
            name: self.number.clone(),
            version_number: self.number.clone(),
            channel: self.channel,
            game_versions: vec![self.game.clone()],
            loaders: self.loaders.clone(),
            date_published: self.date.clone(),
            downloads: 0,
            files: vec![VersionFile {
                filename: self.filename.clone(),
                url: self.url(),
                size: self.bytes.len() as u64,
                sha1: Some(sha1(&self.bytes)),
                sha512: Some(sha512(&self.bytes)),
                primary: true,
            }],
            dependencies: self.deps.clone(),
        }
    }
}

#[derive(Default)]
struct Catalog {
    projects: Vec<Project>,
    versions: Vec<Ver>,
    lookup_calls: AtomicU32,
    update_calls: AtomicU32,
    /// The filter and channels of each update request.
    update_asks: Mutex<Vec<(VersionFilter, Vec<ReleaseChannel>)>>,
    /// Acts as CurseForge: files are found by fingerprint.
    curseforge: bool,
}

/// The CurseForge fingerprint of bytes, as a lookup key.
fn print_of(bytes: &[u8]) -> String {
    vl_curseforge::fingerprint(bytes).to_string()
}

impl Catalog {
    fn project(mut self, id: &str) -> Self {
        self.projects.push(project(id, ProjectType::Mod));
        self
    }

    fn typed(mut self, id: &str, kind: ProjectType) -> Self {
        self.projects.push(project(id, kind));
        self
    }

    fn version(mut self, v: Ver) -> Self {
        self.versions.push(v);
        self
    }

    /// The catalog as CurseForge: provider and lookups by fingerprint.
    fn curseforge(mut self) -> Self {
        self.curseforge = true;
        self
    }

    /// The key a version's file is found by.
    fn key_of(&self, v: &ProjectVersion) -> Option<String> {
        if self.curseforge {
            let ver = self.versions.iter().find(|x| x.id == v.id)?;
            Some(print_of(&ver.bytes))
        } else {
            v.files[0].sha1.clone()
        }
    }

    fn algorithm(&self) -> HashAlgorithm {
        if self.curseforge {
            HashAlgorithm::Fingerprint
        } else {
            HashAlgorithm::Sha1
        }
    }

    fn files(&self) -> HashMap<String, Vec<u8>> {
        self.versions
            .iter()
            .map(|v| (v.url(), v.bytes.clone()))
            .collect()
    }
}

impl CatalogSource for Catalog {
    fn provider(&self) -> Provider {
        if self.curseforge {
            Provider::CurseForge
        } else {
            Provider::Modrinth
        }
    }

    fn file_hash(&self) -> HashAlgorithm {
        self.algorithm()
    }

    fn search(&self, _: &SearchQuery, _: &Control) -> Result<SearchPage> {
        Ok(SearchPage::default())
    }

    fn project(&self, id: &str, _: &Control) -> Result<Project> {
        self.projects
            .iter()
            .find(|p| p.id == id || p.slug == id)
            .cloned()
            .ok_or_else(|| Error::not_found(format!("project {id}")))
    }

    // Ignores the filter on purpose: the planner must not trust it.
    fn versions(
        &self,
        project: &str,
        _: &VersionFilter,
        _: &Control,
    ) -> Result<Vec<ProjectVersion>> {
        Ok(self
            .versions
            .iter()
            .filter(|v| v.project == project)
            .map(Ver::build)
            .collect())
    }

    fn latest_versions_by_hashes(
        &self,
        algorithm: HashAlgorithm,
        hashes: &[String],
        filter: &VersionFilter,
        channels: &[ReleaseChannel],
        _: &Control,
    ) -> Result<Vec<HashMatch>> {
        assert_eq!(algorithm, self.algorithm());
        self.update_calls.fetch_add(1, Ordering::Relaxed);
        self.update_asks
            .lock()
            .unwrap()
            .push((filter.clone(), channels.to_vec()));
        let all: Vec<ProjectVersion> = self.versions.iter().map(Ver::build).collect();
        Ok(all
            .iter()
            .filter_map(|owner| {
                let sha = self.key_of(owner)?;
                if !hashes.contains(&sha) {
                    return None;
                }
                let newest = all
                    .iter()
                    .filter(|v| v.project_id == owner.project_id)
                    .filter(|v| {
                        filter.loaders.is_empty()
                            || v.loaders.iter().any(|l| filter.loaders.contains(l))
                    })
                    .filter(|v| {
                        filter.game_versions.is_empty()
                            || v.game_versions
                                .iter()
                                .any(|g| filter.game_versions.contains(g))
                    })
                    .filter(|v| channels.is_empty() || channels.contains(&v.channel))
                    .max_by(|a, b| a.date_published.cmp(&b.date_published))?;
                Some(HashMatch {
                    hash: sha,
                    version: newest.clone(),
                })
            })
            .collect())
    }

    fn versions_by_hashes(
        &self,
        algorithm: HashAlgorithm,
        hashes: &[String],
        _: &Control,
    ) -> Result<Vec<HashMatch>> {
        assert_eq!(algorithm, self.algorithm());
        self.lookup_calls.fetch_add(1, Ordering::Relaxed);
        Ok(self
            .versions
            .iter()
            .map(Ver::build)
            .filter_map(|v| {
                let sha = self.key_of(&v)?;
                hashes.contains(&sha).then_some(HashMatch {
                    hash: sha,
                    version: v,
                })
            })
            .collect())
    }
}

/// Serves bytes by URL.
struct Files {
    files: Mutex<HashMap<String, Vec<u8>>>,
    fetched: Mutex<Vec<String>>,
}

impl HttpTransport for Files {
    fn get(&self, request: &HttpRequest) -> std::result::Result<HttpResponse, NetError> {
        self.fetched.lock().unwrap().push(request.url.clone());
        let body = self.files.lock().unwrap().get(&request.url).cloned();
        Ok(match body {
            Some(b) => HttpResponse {
                status: 200,
                headers: Vec::new(),
                body: Box::new(Cursor::new(b)),
            },
            None => HttpResponse {
                status: 404,
                headers: Vec::new(),
                body: Box::new(Cursor::new(Vec::new())),
            },
        })
    }
}

struct World {
    dir: Dir,
    catalog: Catalog,
    transport: Arc<Files>,
    client: Client,
    resources: Resources,
    origins: OriginStore,
    control: Control,
}

impl World {
    fn new(catalog: Catalog) -> Self {
        let dir = Dir::new();
        let transport = Arc::new(Files {
            files: Mutex::new(catalog.files()),
            fetched: Mutex::default(),
        });
        let client = Client::with_transport(NetConfig::new("0.0.0"), transport.clone());
        let resources = Resources::new(dir.path().join("game"), dir.path().join("trash"));
        let origins = OriginStore::new(dir.path().join(".vibelauncher").join("resources.json"));
        Self {
            dir,
            catalog,
            transport,
            client,
            resources,
            origins,
            control: Control::default(),
        }
    }

    fn plan_with(
        &self,
        target: &Target,
        project: &str,
        version: Option<&str>,
        min: MinStability,
    ) -> Result<InstallPlan> {
        let input = PlanInput {
            source: &self.catalog,
            resources: &self.resources,
            origins: &self.origins,
            target,
            control: &self.control,
        };
        plan(&input, project, version, min)
    }

    fn plan(&self, project: &str) -> Result<InstallPlan> {
        self.plan_with(&fabric(), project, None, MinStability::Beta)
    }

    fn staging(&self) -> PathBuf {
        self.dir.path().join(".vibelauncher").join("staging")
    }

    fn install(&self, plan: &InstallPlan) -> Result<crate::Report> {
        let staging = self.staging();
        let input = ApplyInput {
            client: &self.client,
            resources: &self.resources,
            origins: &self.origins,
            staging: &staging,
        };
        execute(&input, plan, &self.control)
    }

    fn mods_dir(&self) -> PathBuf {
        self.resources.dir(ResourceKind::Mods)
    }

    fn put_mod(&self, name: &str, bytes: &[u8]) {
        std::fs::create_dir_all(self.mods_dir()).unwrap();
        std::fs::write(self.mods_dir().join(name), bytes).unwrap();
    }

    fn mod_files(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.mods_dir())
            .map(|r| {
                r.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }
}

fn fabric() -> Target {
    Target {
        game_version: "1.21.4".into(),
        loader: Some("fabric".into()),
        world: false,
    }
}

fn titles(plan: &InstallPlan) -> Vec<String> {
    plan.items.iter().map(|i| i.title.clone()).collect()
}

#[test]
fn prefers_release_over_a_newer_beta_and_the_newest_release() {
    let catalog = Catalog::default()
        .project("Sodium")
        .version(ver("v1", "Sodium", "1.0").date("2025-01-01T00:00:00Z"))
        .version(ver("v2", "Sodium", "1.1").date("2025-02-01T00:00:00Z"))
        .version(
            ver("v3", "Sodium", "2.0-beta")
                .channel(ReleaseChannel::Beta)
                .date("2025-03-01T00:00:00Z"),
        )
        .version(
            ver("v4", "Sodium", "3.0-alpha")
                .channel(ReleaseChannel::Alpha)
                .date("2025-04-01T00:00:00Z"),
        );
    let w = World::new(catalog);
    let p = w.plan("sodium").unwrap();
    assert_eq!(p.items.len(), 1);
    assert_eq!(p.items[0].version_id, "v2");
    assert_eq!(p.items[0].action, PlanAction::Install);
    assert_eq!(p.items[0].role, PlanRole::Requested);
    assert_eq!(p.download_bytes(), p.items[0].file.size);
}

#[test]
fn falls_back_to_beta_only_when_allowed() {
    let catalog = Catalog::default()
        .project("Sodium")
        .version(
            ver("v1", "Sodium", "1.0-beta")
                .channel(ReleaseChannel::Beta)
                .date("2025-01-01T00:00:00Z"),
        )
        .version(
            ver("v2", "Sodium", "1.1-alpha")
                .channel(ReleaseChannel::Alpha)
                .date("2025-02-01T00:00:00Z"),
        );
    let w = World::new(catalog);
    let strict = w
        .plan_with(&fabric(), "sodium", None, MinStability::Release)
        .unwrap_err();
    assert_eq!(strict.kind, ErrorKind::NotFound);
    assert!(strict.message.contains("1 beta, 1 alpha"), "{strict}");
    let beta = w.plan("sodium").unwrap();
    assert_eq!(beta.items[0].version_id, "v1");
    let alpha = w
        .plan_with(&fabric(), "sodium", None, MinStability::Alpha)
        .unwrap();
    assert_eq!(alpha.items[0].version_id, "v1", "beta beats a newer alpha");
}

#[test]
fn ignores_versions_for_other_games_and_loaders_and_honors_a_pinned_version() {
    let catalog = Catalog::default()
        .project("Sodium")
        .version(ver("old", "Sodium", "0.5").game("1.20.1"))
        .version(ver("forge", "Sodium", "0.6").loaders(&["forge"]))
        .version(ver("ok", "Sodium", "0.6").date("2025-01-01T00:00:00Z"))
        .version(ver("older", "Sodium", "0.4").date("2024-01-01T00:00:00Z"));
    let w = World::new(catalog);
    assert_eq!(w.plan("sodium").unwrap().items[0].version_id, "ok");
    let pinned = w
        .plan_with(&fabric(), "sodium", Some("older"), MinStability::Release)
        .unwrap();
    assert_eq!(pinned.items[0].version_id, "older");
    let err = w
        .plan_with(&fabric(), "sodium", Some("old"), MinStability::Beta)
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);
    let none = Target {
        game_version: "1.19".into(),
        loader: Some("fabric".into()),
        world: false,
    };
    let err = w
        .plan_with(&none, "sodium", None, MinStability::Beta)
        .unwrap_err();
    assert!(err.message.contains("Minecraft 1.19 on fabric"), "{err}");
}

#[test]
fn quilt_takes_fabric_mods_but_prefers_its_own() {
    let catalog = Catalog::default()
        .project("A")
        .project("B")
        .version(ver("a-fabric", "A", "1.0"))
        .version(ver("b-fabric", "B", "1.0").date("2025-01-01T00:00:00Z"))
        .version(
            ver("b-quilt", "B", "0.9")
                .loaders(&["quilt"])
                .date("2024-01-01T00:00:00Z"),
        );
    let w = World::new(catalog);
    let quilt = Target {
        game_version: "1.21.4".into(),
        loader: Some("quilt".into()),
        world: false,
    };
    let a = w.plan_with(&quilt, "a", None, MinStability::Beta).unwrap();
    assert_eq!(a.items[0].version_id, "a-fabric");
    let b = w.plan_with(&quilt, "b", None, MinStability::Beta).unwrap();
    assert_eq!(b.items[0].version_id, "b-quilt");
}

#[test]
fn a_mod_needs_a_loader_but_a_pack_does_not() {
    let pack = ver("p1", "Pack", "1")
        .loaders(&["minecraft"])
        .file_name("pack.zip");
    let catalog = Catalog::default()
        .project("Sodium")
        .typed("Pack", ProjectType::ResourcePack)
        .typed("Bundle", ProjectType::Modpack)
        .version(ver("v1", "Sodium", "1.0"))
        .version(pack);
    let w = World::new(catalog);
    let vanilla = Target {
        game_version: "1.21.4".into(),
        loader: None,
        world: false,
    };
    let err = w
        .plan_with(&vanilla, "sodium", None, MinStability::Beta)
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidInput);
    let p = w
        .plan_with(&vanilla, "pack", None, MinStability::Beta)
        .unwrap();
    assert_eq!(p.items[0].kind, ResourceKind::ResourcePacks);
    let err = w.plan("bundle").unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsupported);
}

#[test]
fn follows_required_dependencies_recursively_and_survives_cycles() {
    let catalog = Catalog::default()
        .project("A")
        .project("B")
        .project("C")
        .project("D")
        .version(
            ver("a1", "A", "1")
                .dep("B", DependencyKind::Required)
                .dep("D", DependencyKind::Optional)
                .dep("A", DependencyKind::Embedded),
        )
        .version(ver("b1", "B", "1").dep("C", DependencyKind::Required))
        // C needs A again: a cycle.
        .version(ver("c1", "C", "1").dep("A", DependencyKind::Required))
        .version(ver("d1", "D", "1"));
    let w = World::new(catalog);
    let p = w.plan("a").unwrap();
    assert_eq!(titles(&p), ["C", "B", "A"], "dependencies come first");
    assert_eq!(
        p.items[1].role,
        PlanRole::Dependency {
            required_by: "A".into()
        }
    );
    assert_eq!(p.recommended.len(), 1);
    assert_eq!(p.recommended[0].title.as_deref(), Some("D"));
    assert_eq!(p.recommended[0].suggested_by, "A");
    // Nothing optional is planned.
    assert!(p.items.iter().all(|i| i.title != "D"));
}

#[test]
fn a_dependency_without_a_fitting_version_fails_the_plan() {
    let catalog = Catalog::default()
        .project("A")
        .project("B")
        .version(ver("a1", "A", "1").dep("B", DependencyKind::Required))
        .version(ver("b1", "B", "1").game("1.20.1"));
    let w = World::new(catalog);
    let err = w.plan("a").unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);
    assert!(err.message.contains("B for Minecraft 1.21.4"), "{err}");
}

#[test]
fn installed_dependencies_are_recognized_by_hash_origin_or_mod_id() {
    let b = ver("b1", "B", "1");
    let catalog = Catalog::default()
        .project("A")
        .project("B")
        .project("C")
        .project("D")
        .version(
            ver("a1", "A", "1")
                .dep("B", DependencyKind::Required)
                .dep("C", DependencyKind::Required)
                .dep("D", DependencyKind::Required),
        )
        .version(b.clone())
        .version(ver("c1", "C", "1"))
        .version(ver("d1", "D", "1"));
    let w = World::new(catalog);
    // B: the catalog knows this file by hash, under another name.
    w.put_mod("renamed-b.jar", &b.bytes);
    // C: only the origin record knows it (its hash is not in the catalog).
    let c_local = jar("c", "local-build");
    w.put_mod("c-local.jar", &c_local);
    w.origins
        .save(&[Origin {
            kind: ResourceKind::Mods,
            file: "c-local.jar".into(),
            sha1: sha1(&c_local),
            sha512: None,
            size: c_local.len() as u64,
            provider: Provider::Modrinth,
            project_id: "C".into(),
            slug: Some("c".into()),
            version_id: "c1".into(),
            version_number: Some("1".into()),
            installed_unix: 1,
            dependency: false,
        }])
        .unwrap();
    // D: a jar from elsewhere whose mod id equals the slug.
    w.put_mod("d-custom.jar", &jar("d", "9"));
    let p = w.plan("a").unwrap();
    assert_eq!(titles(&p), ["A"]);
    let how: HashMap<_, _> = p
        .satisfied
        .iter()
        .map(|s| (s.title.as_str(), (s.file.as_str(), s.matched_by)))
        .collect();
    assert_eq!(how["B"], ("renamed-b.jar", MatchedBy::Hash));
    assert_eq!(how["C"], ("c-local.jar", MatchedBy::Origin));
    assert_eq!(how["D"], ("d-custom.jar", MatchedBy::ModId));
}

#[test]
fn replaces_another_version_and_keeps_the_same_one() {
    let old = ver("v1", "A", "1");
    let catalog = Catalog::default()
        .project("A")
        .version(old.clone())
        .version(ver("v2", "A", "2").date("2025-02-01T00:00:00Z"));
    let w = World::new(catalog);
    w.put_mod("a-1.jar", &old.bytes);
    let p = w.plan("a").unwrap();
    assert_eq!(
        p.items[0].action,
        PlanAction::Replace {
            file: "a-1.jar".into(),
            old_version: Some("1".into())
        }
    );
    let pinned = w
        .plan_with(&fabric(), "a", Some("v1"), MinStability::Beta)
        .unwrap();
    assert_eq!(
        pinned.items[0].action,
        PlanAction::Present {
            file: "a-1.jar".into()
        }
    );
    assert_eq!(pinned.download_bytes(), 0);
}

#[test]
fn incompatible_mods_are_a_clear_conflict() {
    let rival = ver("r1", "Rival", "1");
    let catalog = Catalog::default()
        .project("A")
        .project("Rival")
        .project("B")
        .version(ver("a1", "A", "1").dep("Rival", DependencyKind::Incompatible))
        .version(rival.clone())
        .version(
            ver("b1", "B", "1")
                .dep("Rival", DependencyKind::Required)
                .dep("A", DependencyKind::Required),
        );
    let w = World::new(catalog);
    // The new mod declares the installed one incompatible.
    w.put_mod("rival.jar", &rival.bytes);
    let err = w.plan("a").unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);
    assert!(
        err.message.contains("incompatible") && err.message.contains("rival.jar"),
        "{err}"
    );
    std::fs::remove_file(w.mods_dir().join("rival.jar")).unwrap();
    assert!(w.plan("a").is_ok());
    // Two parts of one install that exclude each other.
    let err = w.plan("b").unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);
    assert!(err.message.contains("incompatible"), "{err}");
}

#[test]
fn an_installed_mod_that_declares_the_new_one_incompatible_blocks_it() {
    let old = ver("x1", "X", "1").dep("A", DependencyKind::Incompatible);
    let catalog = Catalog::default()
        .project("A")
        .project("X")
        .version(ver("a1", "A", "1"))
        .version(old.clone());
    let w = World::new(catalog);
    w.put_mod("x.jar", &old.bytes);
    let err = w.plan("a").unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);
    assert!(err.message.contains("x.jar"), "{err}");
}

#[test]
fn a_foreign_file_with_the_same_name_is_a_conflict() {
    let catalog = Catalog::default().project("A").version(ver("a1", "A", "1"));
    let w = World::new(catalog);
    w.put_mod("a-1.jar", b"something else");
    let err = w.plan("a").unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);
    assert!(err.message.contains("a-1.jar"), "{err}");
}

#[test]
fn rejects_files_the_folder_cannot_take() {
    let catalog = Catalog::default()
        .project("A")
        .version(ver("a1", "A", "1").file_name("a.exe"));
    let w = World::new(catalog);
    assert_eq!(w.plan("a").unwrap_err().kind, ErrorKind::Unsupported);
}

#[test]
fn installs_with_dependencies_records_origins_and_a_second_run_does_nothing() {
    let catalog = Catalog::default()
        .project("A")
        .project("B")
        .version(ver("a1", "A", "1").dep("B", DependencyKind::Required))
        .version(ver("b1", "B", "1"));
    let w = World::new(catalog);
    let p = w.plan("a").unwrap();
    let report = w.install(&p).unwrap();
    assert_eq!(w.mod_files(), ["a-1.jar", "b-1.jar"]);
    assert_eq!(report.added.len(), 2);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert!(!w.staging().exists(), "scratch files are removed");
    assert_eq!(w.transport.fetched.lock().unwrap().len(), 2);

    let loaded = w.origins.load();
    assert!(!loaded.damaged);
    assert_eq!(loaded.origins.len(), 2);
    let a = loaded.origins.iter().find(|o| o.project_id == "A").unwrap();
    assert_eq!(
        (a.version_id.as_str(), a.file.as_str(), a.dependency),
        ("a1", "a-1.jar", false)
    );
    assert_eq!(a.sha1, sha1(&jar("a", "1")));
    assert!(
        loaded
            .origins
            .iter()
            .any(|o| o.project_id == "B" && o.dependency)
    );

    // Planning again finds everything in place.
    let again = w.plan("a").unwrap();
    assert!(again.items.iter().all(|i| !i.downloads()));
    assert_eq!(again.download_bytes(), 0);
    assert!(w.install(&again).unwrap().added.is_empty());
}

#[test]
fn replacing_trashes_the_old_file_and_keeps_it_disabled_if_it_was() {
    let old = ver("v1", "A", "1");
    let catalog = Catalog::default()
        .project("A")
        .version(old.clone())
        .version(ver("v2", "A", "2").date("2025-02-01T00:00:00Z"));
    let w = World::new(catalog);
    w.put_mod("a-1.jar.disabled", &old.bytes);
    let p = w.plan("a").unwrap();
    assert!(matches!(p.items[0].action, PlanAction::Replace { .. }));
    let report = w.install(&p).unwrap();
    assert_eq!(
        report.replaced,
        [(ResourceKind::Mods, "a-1.jar".to_owned())]
    );
    assert_eq!(w.mod_files(), ["a-2.jar.disabled"]);
    let trash: Vec<_> = std::fs::read_dir(w.dir.path().join("trash"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(trash.len(), 1);
    let origins = w.origins.load().origins;
    assert_eq!(origins.len(), 1);
    assert_eq!(origins[0].version_id, "v2");
}

#[test]
fn a_tampered_download_adds_nothing() {
    let catalog = Catalog::default()
        .project("A")
        .project("B")
        .version(ver("a1", "A", "1").dep("B", DependencyKind::Required))
        .version(ver("b1", "B", "1"));
    let w = World::new(catalog);
    let p = w.plan("a").unwrap();
    // Same length, other content: only the hash can tell.
    let url = p.items[1].file.url.clone();
    let mut tampered = jar("a", "1");
    let last = tampered.len() - 1;
    tampered[last] ^= 0xff;
    w.transport.files.lock().unwrap().insert(url, tampered);
    let err = w.install(&p).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Integrity, "{err}");
    assert!(w.mod_files().is_empty());
    assert!(!w.origins.path().exists());
}

#[test]
fn a_failure_after_the_first_file_puts_the_folder_back() {
    let old = ver("v1", "A", "1");
    let catalog = Catalog::default()
        .project("A")
        .project("B")
        .version(old.clone())
        .version(
            ver("v2", "A", "2")
                .date("2025-02-01T00:00:00Z")
                .dep("B", DependencyKind::Required),
        )
        .version(ver("b1", "B", "1"));
    let w = World::new(catalog);
    w.put_mod("a-1.jar", &old.bytes);
    let p = w.plan("a").unwrap();
    assert_eq!(titles(&p), ["B", "A"]);
    // B is added first; then A's new name turns out to be taken.
    w.put_mod("a-2.jar", b"a late arrival");
    let err = w.install(&p).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict, "{err}");
    assert_eq!(w.mod_files(), ["a-1.jar", "a-2.jar"]);
    assert_eq!(
        std::fs::read(w.mods_dir().join("a-1.jar")).unwrap(),
        old.bytes
    );
    assert!(!w.origins.path().exists());
}

#[test]
fn cancelling_stops_before_anything_is_added() {
    let catalog = Catalog::default().project("A").version(ver("a1", "A", "1"));
    let mut w = World::new(catalog);
    let token = CancelToken::new();
    w.control = Control::with_cancel(token.clone());
    let p = w.plan("a").unwrap();
    token.cancel();
    let err = w.install(&p).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Cancelled);
    assert!(w.mod_files().is_empty());
}

#[test]
fn origin_records_survive_damage_and_manual_changes() {
    let dir = Dir::new();
    let store = OriginStore::new(dir.path().join("sub").join("resources.json"));
    assert!(store.load().origins.is_empty() && !store.load().damaged);

    let origin = |file: &str, sha: &str| Origin {
        kind: ResourceKind::Mods,
        file: file.to_owned(),
        sha1: sha.to_owned(),
        sha512: None,
        size: 1,
        provider: Provider::Modrinth,
        project_id: "P".into(),
        slug: None,
        version_id: "V".into(),
        version_number: None,
        installed_unix: 0,
        dependency: false,
    };
    store.save(&[origin("a.jar", &"a".repeat(40))]).unwrap();

    // One entry is garbage, one has an unknown extra field: only the garbage is skipped.
    let text = std::fs::read_to_string(store.path()).unwrap();
    let mut doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    doc["resources"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"kind": 7}));
    doc["resources"][0]["future_field"] = serde_json::json!(true);
    std::fs::write(store.path(), doc.to_string()).unwrap();
    let loaded = store.load();
    assert_eq!(loaded.origins.len(), 1);
    assert!(loaded.damaged);

    // Not JSON at all.
    std::fs::write(store.path(), b"{ not json").unwrap();
    let loaded = store.load();
    assert!(loaded.origins.is_empty() && loaded.damaged);
    // Recording over it keeps a copy of the damaged file.
    store
        .record(vec![origin("b.jar", &"b".repeat(40))], |_| true, &[])
        .unwrap();
    assert!(!store.load().damaged);
    assert!(dir.path().join("sub").join("resources.json.bad").exists());

    // A record is replaced by the same file name or the same hash, dropped when its file is gone.
    store
        .record(vec![origin("b.jar", &"c".repeat(40))], |_| true, &[])
        .unwrap();
    assert_eq!(store.load().origins.len(), 1);
    store
        .record(
            vec![origin("d.jar", &"d".repeat(40))],
            |o| o.file == "d.jar",
            &[],
        )
        .unwrap();
    let files: Vec<_> = store.load().origins.into_iter().map(|o| o.file).collect();
    assert_eq!(files, ["d.jar"]);
}

mod combined_cases;
mod updates_cases;
mod world_cases;
