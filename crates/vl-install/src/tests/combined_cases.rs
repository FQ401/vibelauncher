//! Update checks and updates across Modrinth and CurseForge at once.

use super::*;
use crate::updates::{Selection, UpdateInput, UpdateOptions, check_all, plan_update_all};
use vl_core::install::UncheckedReason;

fn meta(w: &World) -> PathBuf {
    w.dir.path().join(".vibelauncher")
}

fn input<'a>(
    w: &'a World,
    source: &'a dyn CatalogSource,
    target: &'a Target,
    meta: &'a Path,
) -> UpdateInput<'a> {
    UpdateInput {
        source,
        resources: &w.resources,
        origins: &w.origins,
        target,
        meta_dir: meta,
        control: &w.control,
    }
}

/// A record of `file` with these bytes, installed from `provider`.
fn recorded(file: &str, bytes: &[u8], provider: Provider, project: &str, version: &str) -> Origin {
    Origin {
        kind: ResourceKind::Mods,
        file: file.to_owned(),
        sha1: sha1(bytes),
        sha512: None,
        size: bytes.len() as u64,
        provider,
        project_id: project.to_owned(),
        slug: None,
        version_id: version.to_owned(),
        version_number: None,
        installed_unix: 0,
        dependency: false,
    }
}

const LATER: &str = "2025-02-01T00:00:00Z";

/// Modrinth knows `a` and `b`; CurseForge knows the same files of `a` and `b` and also `c`.
/// Both have a newer version of each.
fn both() -> (World, Catalog, [Ver; 3]) {
    let a1 = ver("mr-a1", "A", "1");
    let b1 = ver("mr-b1", "B", "1");
    let modrinth = Catalog::default()
        .project("A")
        .project("B")
        .version(a1.clone())
        .version(ver("mr-a2", "A", "2").date(LATER))
        .version(b1.clone())
        .version(ver("mr-b2", "B", "2").date(LATER));
    // Same names and bytes as Modrinth's files (the bytes come from the lowercase name).
    let c1 = ver("31", "c", "1");
    let curseforge = Catalog::default()
        .curseforge()
        .project("a")
        .project("b")
        .project("c")
        .version(ver("11", "a", "1"))
        .version(ver("12", "a", "2").date(LATER))
        .version(ver("21", "b", "1"))
        .version(ver("22", "b", "2").date(LATER))
        .version(c1.clone())
        .version(ver("32", "c", "2").date(LATER));
    let w = World::new(modrinth);
    w.transport.files.lock().unwrap().extend(curseforge.files());
    (w, curseforge, [a1, b1, c1])
}

#[test]
fn a_file_both_platforms_know_is_offered_once_and_records_decide() {
    let (w, curseforge, [a1, b1, c1]) = both();
    assert_eq!(
        a1.bytes,
        ver("11", "a", "1").bytes,
        "the platforms share a file"
    );
    w.put_mod("a-1.jar", &a1.bytes);
    w.put_mod("b-1.jar", &b1.bytes);
    w.put_mod("c-1.jar.disabled", &c1.bytes);
    w.put_mod("mine.jar", &jar("mine", "1"));
    // b was installed from CurseForge: it is CurseForge's although Modrinth knows it too.
    w.origins
        .save(&[recorded(
            "b-1.jar",
            &b1.bytes,
            Provider::CurseForge,
            "b",
            "21",
        )])
        .unwrap();
    let (target, meta) = (fabric(), meta(&w));
    let inputs = [
        input(&w, &w.catalog, &target, &meta),
        input(&w, &curseforge, &target, &meta),
    ];
    let report = check_all(&inputs, UpdateOptions::default()).unwrap();
    let mut offered: Vec<(&str, Provider, &str)> = report
        .updates
        .iter()
        .map(|u| (u.file.as_str(), u.provider, u.latest.id.as_str()))
        .collect();
    offered.sort_by_key(|o| o.0);
    assert_eq!(
        offered,
        [
            ("a-1.jar", Provider::Modrinth, "mr-a2"),
            ("b-1.jar", Provider::CurseForge, "22"),
            ("c-1.jar", Provider::CurseForge, "32"),
        ]
    );
    assert_eq!(report.up_to_date, 0);
    assert_eq!(report.unchecked.len(), 1, "{:?}", report.unchecked);
    assert_eq!(report.unchecked[0].file, "mine.jar");
    assert_eq!(report.unchecked[0].reason, UncheckedReason::NoOrigin);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    // The update takes each file from the platform that offered it.
    let plan = plan_update_all(&inputs, UpdateOptions::default(), Selection::All, None).unwrap();
    let mut items: Vec<(&str, Provider)> = plan
        .items
        .iter()
        .map(|i| (i.version_id.as_str(), i.provider))
        .collect();
    items.sort_by_key(|i| i.0);
    assert_eq!(
        items,
        [
            ("22", Provider::CurseForge),
            ("32", Provider::CurseForge),
            ("mr-a2", Provider::Modrinth),
        ]
    );
    w.install(&plan).unwrap();
    assert_eq!(
        w.mod_files(),
        ["a-2.jar", "b-2.jar", "c-2.jar.disabled", "mine.jar"]
    );
    let mut records: Vec<(String, Provider)> = w
        .origins
        .load()
        .origins
        .into_iter()
        .map(|o| (o.file, o.provider))
        .collect();
    records.sort_by(|x, y| x.0.cmp(&y.0));
    assert_eq!(
        records,
        [
            ("a-2.jar".to_owned(), Provider::Modrinth),
            ("b-2.jar".to_owned(), Provider::CurseForge),
            ("c-2.jar".to_owned(), Provider::CurseForge),
        ]
    );
    let again = check_all(&inputs, UpdateOptions::default()).unwrap();
    assert!(again.updates.is_empty(), "{:?}", again.updates);
    assert_eq!(again.up_to_date, 3);
}

#[test]
fn files_are_selected_on_the_platform_that_takes_them() {
    let (w, curseforge, [a1, b1, c1]) = both();
    w.put_mod("a-1.jar", &a1.bytes);
    w.put_mod("b-1.jar", &b1.bytes);
    w.put_mod("c-1.jar", &c1.bytes);
    w.put_mod("mine.jar", &jar("mine", "1"));
    let (target, meta) = (fabric(), meta(&w));
    let inputs = [
        input(&w, &w.catalog, &target, &meta),
        input(&w, &curseforge, &target, &meta),
    ];
    let files = ["a-1.jar".to_owned(), "c-1.jar".to_owned()];
    let plan = plan_update_all(
        &inputs,
        UpdateOptions::default(),
        Selection::Files(&files),
        None,
    )
    .unwrap();
    let mut items: Vec<&str> = plan.items.iter().map(|i| i.version_id.as_str()).collect();
    items.sort_unstable();
    assert_eq!(items, ["32", "mr-a2"]);

    // A version switch goes to the platform that takes the file.
    let one = ["c-1.jar".to_owned()];
    let plan = plan_update_all(
        &inputs,
        UpdateOptions::default(),
        Selection::Files(&one),
        Some("32"),
    )
    .unwrap();
    assert_eq!(plan.items.len(), 1);
    assert_eq!(plan.items[0].provider, Provider::CurseForge);

    let mine = ["mine.jar".to_owned()];
    let err = plan_update_all(
        &inputs,
        UpdateOptions::default(),
        Selection::Files(&mine),
        None,
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsupported);
    let nothing = ["nothing.jar".to_owned()];
    let err = plan_update_all(
        &inputs,
        UpdateOptions::default(),
        Selection::Files(&nothing),
        None,
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);
}

/// CurseForge without a key.
struct NoKey;

impl CatalogSource for NoKey {
    fn provider(&self) -> Provider {
        Provider::CurseForge
    }

    fn file_hash(&self) -> HashAlgorithm {
        HashAlgorithm::Fingerprint
    }

    fn search(&self, _: &SearchQuery, _: &Control) -> Result<SearchPage> {
        Err(vl_curseforge::no_api_key())
    }

    fn project(&self, _: &str, _: &Control) -> Result<Project> {
        Err(vl_curseforge::no_api_key())
    }

    fn versions(&self, _: &str, _: &VersionFilter, _: &Control) -> Result<Vec<ProjectVersion>> {
        Err(vl_curseforge::no_api_key())
    }

    fn versions_by_hashes(
        &self,
        _: HashAlgorithm,
        _: &[String],
        _: &Control,
    ) -> Result<Vec<HashMatch>> {
        Err(vl_curseforge::no_api_key())
    }
}

#[test]
fn a_platform_that_cannot_be_asked_is_skipped_with_a_warning() {
    let (w, _, [a1, b1, _]) = both();
    w.put_mod("a-1.jar", &a1.bytes);
    w.put_mod("b-1.jar", &b1.bytes);
    w.origins
        .save(&[recorded(
            "b-1.jar",
            &b1.bytes,
            Provider::CurseForge,
            "b",
            "21",
        )])
        .unwrap();
    let (target, meta) = (fabric(), meta(&w));
    let inputs = [
        input(&w, &w.catalog, &target, &meta),
        input(&w, &NoKey, &target, &meta),
    ];
    let report = check_all(&inputs, UpdateOptions::default()).unwrap();
    assert_eq!(report.updates.len(), 1);
    assert_eq!(report.updates[0].provider, Provider::Modrinth);
    assert_eq!(report.unchecked.len(), 1);
    assert_eq!(report.unchecked[0].file, "b-1.jar");
    assert_eq!(report.unchecked[0].reason, UncheckedReason::OtherPlatform);
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(report.warnings[0].starts_with("CurseForge was not checked"));

    let plan = plan_update_all(&inputs, UpdateOptions::default(), Selection::All, None).unwrap();
    assert_eq!(plan.items.len(), 1);
    let b = ["b-1.jar".to_owned()];
    let err = plan_update_all(
        &inputs,
        UpdateOptions::default(),
        Selection::Files(&b),
        None,
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsupported);
    assert!(err.message.contains("CurseForge"), "{}", err.message);

    // Nobody to ask: the error of the platform.
    let only = [input(&w, &NoKey, &target, &meta)];
    let err = check_all(&only, UpdateOptions::default()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::NoApiKey);
}

#[test]
fn a_repeated_check_takes_fingerprints_and_hashes_from_the_folder_cache() {
    let (w, curseforge, [_, _, c1]) = both();
    let resources = Resources::new(w.dir.path().join("game"), w.dir.path().join("trash"))
        .with_cache_dir(w.dir.path().join(".vibelauncher"));
    let path = w.mods_dir().join("c-1.jar");
    w.put_mod("c-1.jar", &c1.bytes);
    let (target, meta) = (fabric(), meta(&w));
    let ask = |resources: &Resources| {
        let inputs = [
            UpdateInput {
                resources,
                ..input(&w, &w.catalog, &target, &meta)
            },
            UpdateInput {
                resources,
                ..input(&w, &curseforge, &target, &meta)
            },
        ];
        check_all(&inputs, UpdateOptions::default()).unwrap()
    };
    let first = ask(&resources);
    assert_eq!(first.updates.len(), 1);
    assert_eq!(first.updates[0].provider, Provider::CurseForge);

    // Other bytes of the same length and time: only a fresh read would notice.
    let stamp = std::fs::metadata(&path).unwrap().modified().unwrap();
    let mut other = c1.bytes.clone();
    other.iter_mut().for_each(|b| *b = b.wrapping_add(1));
    std::fs::write(&path, &other).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(stamp)
        .unwrap();
    assert_eq!(ask(&resources), first);

    // Without the cache the file is read and no longer known.
    let fresh = ask(&w.resources);
    assert!(fresh.updates.is_empty(), "{fresh:?}");
}
