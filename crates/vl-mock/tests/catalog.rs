//! The mock's catalog, installs, updates and modpacks through the facade only.
#![allow(clippy::panic, clippy::unwrap_used)] // test helpers fail the test by panicking

mod common;

use common::{call, files, id, instances, mock, run_task};
use vl_core::catalog::{
    ApiKeyOrigin, ApiKeyState, CatalogCommand, ProjectType, Provider, SearchQuery, SearchSort,
};
use vl_core::install::{
    ExportInclude, InstallRequest, PackAction, PackExportRequest, PackInstallRequest, PackSource,
    PackUpdateRequest, PlanAction, UpdateCheckRequest, UpdateRequest,
};
use vl_core::instance::InstanceEvent;
use vl_core::log::LogLevel;
use vl_core::resources::ResourceKind;
use vl_core::secret::Secret;
use vl_core::task::{TaskEvent, TaskKind, TaskOutcome};
use vl_core::{Command, Error, ErrorKind, Event, EventStream, Facade, Response};
use vl_mock::{MockFacade, MockOptions};

fn search(facade: &MockFacade, query: SearchQuery) -> Result<vl_core::catalog::SearchPage, Error> {
    match call(facade, Command::Catalog(CatalogCommand::Search(query)))? {
        Response::CatalogSearch(page) => Ok(page),
        other => panic!("unexpected {other:?}"),
    }
}

fn catalog(facade: &MockFacade, command: CatalogCommand) -> Result<Response, Error> {
    call(facade, Command::Catalog(command))
}

fn install_request(instance: &str, provider: Provider, project: &str) -> InstallRequest {
    InstallRequest {
        instance: id(instance),
        provider,
        project: project.into(),
        version: None,
        min_stability: Default::default(),
        world: None,
    }
}

#[test]
fn the_catalog_has_every_type_on_both_platforms_and_pages() {
    let facade = mock();
    for kind in ProjectType::SEARCHABLE {
        for provider in [Provider::Modrinth, Provider::CurseForge] {
            let page = search(
                &facade,
                SearchQuery {
                    provider,
                    project_type: Some(kind),
                    ..SearchQuery::default()
                },
            )
            .unwrap();
            let none_expected = provider == Provider::CurseForge
                && matches!(kind, ProjectType::Shader | ProjectType::DataPack);
            assert_eq!(page.hits.is_empty(), none_expected, "{kind:?} {provider:?}");
            assert!(page.hits.iter().all(|h| h.project_type == kind));
        }
    }
    let query = |offset, limit| SearchQuery {
        query: "mock".into(),
        offset,
        limit,
        ..SearchQuery::default()
    };
    let first = search(&facade, query(0, 5)).unwrap();
    let second = search(&facade, query(5, 5)).unwrap();
    assert_eq!((first.total_hits, first.hits.len()), (12, 5));
    assert_eq!((second.offset, second.hits.len()), (5, 5));
    assert!(
        first
            .hits
            .iter()
            .all(|a| second.hits.iter().all(|b| a.slug != b.slug))
    );
    assert_eq!(search(&facade, query(10, 5)).unwrap().hits.len(), 2);
    assert!(search(&facade, query(99, 5)).unwrap().hits.is_empty());

    let by_downloads = search(
        &facade,
        SearchQuery {
            sort: SearchSort::Downloads,
            ..SearchQuery::default()
        },
    )
    .unwrap();
    assert!(
        by_downloads
            .hits
            .windows(2)
            .all(|w| w[0].downloads >= w[1].downloads)
    );
    for sort in [SearchSort::Follows, SearchSort::Newest, SearchSort::Updated] {
        let page = search(
            &facade,
            SearchQuery {
                sort,
                ..SearchQuery::default()
            },
        )
        .unwrap();
        assert_eq!(page.hits.len(), 12);
    }
    let filtered = search(
        &facade,
        SearchQuery {
            loader: Some("fabric".into()),
            game_version: Some("26.3".into()),
            categories: vec!["utility".into()],
            ..SearchQuery::default()
        },
    )
    .unwrap();
    assert_eq!(filtered.total_hits, 5);
    let no_version = SearchQuery {
        game_version: Some("1.0".into()),
        ..SearchQuery::default()
    };
    assert_eq!(search(&facade, no_version).unwrap().total_hits, 0);
    let bad = SearchQuery {
        limit: 0,
        ..SearchQuery::default()
    };
    assert_eq!(
        search(&facade, bad).unwrap_err().kind,
        ErrorKind::InvalidInput
    );
}

#[test]
fn projects_and_versions_are_found_by_slug_and_id() {
    let facade = mock();
    let project = |key: &str| catalog(&facade, CatalogCommand::Project { id: key.into() });
    let Response::CatalogProject(by_slug) = project("mock-renderer").unwrap() else {
        panic!("expected a project");
    };
    let Response::CatalogProject(by_id) = project(&by_slug.id).unwrap() else {
        panic!("expected a project");
    };
    assert_eq!(by_slug, by_id);
    assert_eq!(by_slug.version_ids.len(), 3);
    assert_eq!(project("nope").unwrap_err().kind, ErrorKind::NotFound);

    let versions = |key: &str, loaders: Vec<String>| {
        catalog(
            &facade,
            CatalogCommand::Versions {
                project: key.into(),
                filter: vl_core::catalog::VersionFilter {
                    loaders,
                    game_versions: vec![],
                },
            },
        )
    };
    let Response::CatalogVersions(all) = versions("mock-tech-pack", vec![]).unwrap() else {
        panic!("expected versions");
    };
    assert_eq!(all.len(), 3);
    assert!(all.iter().all(|v| v.files[0].filename.ends_with(".zip")));
    let Response::CatalogVersions(none) = versions("mock-renderer", vec!["quilt".into()]).unwrap()
    else {
        panic!("expected versions");
    };
    assert!(none.is_empty());
    let Response::CatalogVersions(renderer) = versions("mock-renderer", vec![]).unwrap() else {
        panic!("expected versions");
    };
    assert_eq!(renderer[0].dependencies.len(), 1);
    assert!(renderer[0].files[0].sha512.is_some());
}

#[test]
fn curseforge_needs_the_key_and_modrinth_does_not() {
    let facade = MockFacade::new(MockOptions::instant());
    let cf = SearchQuery {
        provider: Provider::CurseForge,
        ..SearchQuery::default()
    };
    assert_eq!(search(&facade, cf).unwrap_err().kind, ErrorKind::NoApiKey);
    assert!(search(&facade, SearchQuery::default()).is_ok());
    for command in [
        CatalogCommand::Project {
            id: "mock-tech-pack".into(),
        },
        CatalogCommand::Versions {
            project: "mock-tech-pack".into(),
            filter: Default::default(),
        },
        CatalogCommand::PlanInstall(install_request(
            "creative",
            Provider::CurseForge,
            "mock-pocket-storage",
        )),
        CatalogCommand::CheckUpdates(UpdateCheckRequest {
            provider: Some(Provider::CurseForge),
            ..UpdateCheckRequest::new(id("creative"))
        }),
        CatalogCommand::InstallPack(pack_request(Provider::CurseForge, "mock-tech-pack")),
    ] {
        assert_eq!(
            catalog(&facade, command).unwrap_err().kind,
            ErrorKind::NoApiKey
        );
    }
}

#[test]
fn installing_a_mod_adds_its_library_and_the_files() {
    let facade = mock();
    let events = facade.subscribe();
    let request = install_request("survival", Provider::Modrinth, "mock-renderer");
    let Response::InstallPlan(plan) =
        catalog(&facade, CatalogCommand::PlanInstall(request.clone())).unwrap()
    else {
        panic!("expected a plan");
    };
    assert_eq!(plan.items.len(), 2, "the library and the mod");
    assert!(plan.warnings.iter().any(|w| w.contains("no mod loader")));
    assert_eq!(plan.recommended.len(), 1);

    let response = catalog(&facade, CatalogCommand::Install(request.clone())).unwrap();
    let (seen, outcome) = run_task(&events, response);
    assert_eq!(outcome, TaskOutcome::Succeeded);
    assert!(seen.iter().any(|e| matches!(
        e,
        Event::Task(TaskEvent::Started(info)) if info.kind == TaskKind::InstallResource
            && info.subject.as_deref() == Some("Mock Renderer")
    )));
    assert!(seen.iter().any(|e| matches!(
        e,
        Event::Task(TaskEvent::Progress { progress, .. }) if progress.fraction() == Some(1.0)
    )));
    let mods = files(&facade, "survival", ResourceKind::Mods);
    assert!(
        mods.iter()
            .any(|(f, _)| f == "mock-renderer-2.1.0-beta.1.jar")
    );
    assert!(mods.iter().any(|(f, _)| f == "mock-library-2.0.0.jar"));

    // Everything is there now: a plan says so and an install has nothing to do.
    let Response::InstallPlan(again) =
        catalog(&facade, CatalogCommand::PlanInstall(request.clone())).unwrap()
    else {
        panic!("expected a plan");
    };
    assert!(again.downloads().next().is_none());
    assert_eq!(again.satisfied.len(), 1);
    assert_eq!(
        catalog(&facade, CatalogCommand::Install(request))
            .unwrap_err()
            .kind,
        ErrorKind::Conflict
    );
}

#[test]
fn install_requests_are_checked() {
    let facade = mock();
    let plan = |request| catalog(&facade, CatalogCommand::PlanInstall(request));
    let kind = |instance, provider, project| {
        plan(install_request(instance, provider, project))
            .unwrap_err()
            .kind
    };
    let (mr, cf) = (Provider::Modrinth, Provider::CurseForge);
    assert_eq!(kind("nope", mr, "mock-renderer"), ErrorKind::NotFound);
    assert_eq!(kind("broken", mr, "mock-renderer"), ErrorKind::InvalidInput);
    assert_eq!(kind("creative", mr, "nope"), ErrorKind::NotFound);
    assert_eq!(kind("creative", cf, "mock-renderer"), ErrorKind::NotFound);
    assert_eq!(
        kind("creative", mr, "mock-adventure-pack"),
        ErrorKind::InvalidInput
    );
    assert_eq!(
        kind("creative", mr, "mock-recipes-datapack"),
        ErrorKind::InvalidInput
    );
    let mut in_world = install_request("creative", mr, "mock-recipes-datapack");
    in_world.world = Some("nowhere".into());
    assert_eq!(
        plan(in_world.clone()).unwrap_err().kind,
        ErrorKind::NotFound
    );
    let mut renderer = install_request("creative", mr, "mock-renderer");
    renderer.world = Some("creative-world".into());
    assert_eq!(plan(renderer).unwrap_err().kind, ErrorKind::InvalidInput);
    let mut missing = install_request("creative", mr, "mock-renderer");
    missing.version = Some("nope".into());
    assert_eq!(plan(missing).unwrap_err().kind, ErrorKind::NotFound);

    // A data pack goes into a world; the version in the folder is replaced.
    in_world.world = Some("creative-world".into());
    let Response::InstallPlan(plan) = plan(in_world).unwrap() else {
        panic!("expected a plan");
    };
    assert!(matches!(
        &plan.items[0].action,
        PlanAction::Replace { file, old_version: Some(v) }
            if file == "mock-recipes-datapack-1.1.0.zip" && v == "1.1.0"
    ));
}

#[test]
fn updates_are_checked_planned_and_applied() {
    let facade = mock();
    let events = facade.subscribe();
    let check = UpdateCheckRequest {
        provider: Some(Provider::Modrinth),
        ..UpdateCheckRequest::new(id("creative"))
    };
    let report_of = |check: UpdateCheckRequest| {
        let Response::UpdateReport(report) =
            catalog(&facade, CatalogCommand::CheckUpdates(check)).unwrap()
        else {
            panic!("expected a report");
        };
        report
    };
    let report = report_of(check.clone());
    let names: Vec<&str> = report.updates.iter().map(|u| u.file.as_str()).collect();
    assert!(names.contains(&"mock-weather-fx-1.1.0.jar"));
    assert!(names.contains(&"mock-textures-1.1.0.zip"));
    assert!(
        !names.contains(&"mock-renderer-1.1.0.jar"),
        "it came with the pack"
    );
    assert_eq!(report.from_pack.len(), 3);
    assert!(report.unchecked.iter().any(|u| u.file == "local-tweak.jar"));
    assert!(
        report
            .unchecked
            .iter()
            .any(|u| u.file == "mock-pocket-storage-1.1.0.jar")
    );
    assert!(report.up_to_date >= 1);

    let full = report_of(UpdateCheckRequest {
        include_pack_files: true,
        ..check.clone()
    });
    assert!(
        full.updates
            .iter()
            .any(|u| u.file == "mock-renderer-1.1.0.jar" && u.from_pack)
    );
    let vanilla = report_of(UpdateCheckRequest::new(id("survival")));
    assert!(
        vanilla
            .unchecked
            .iter()
            .any(|u| u.file == "stray-library.jar")
    );
    let world = report_of(UpdateCheckRequest {
        world: Some("creative-world".into()),
        ..check.clone()
    });
    assert_eq!(world.updates.len(), 1);

    let update = |files: &[&str], all: bool, version: Option<&str>| UpdateRequest {
        check: check.clone(),
        all,
        files: files.iter().map(|f| (*f).into()).collect(),
        version: version.map(Into::into),
    };
    let plan = |request| catalog(&facade, CatalogCommand::PlanUpdate(request));
    let Response::InstallPlan(all) = plan(update(&[], true, None)).unwrap() else {
        panic!("expected a plan");
    };
    assert_eq!(all.items.len(), report.updates.len());
    let kind = |request| plan(request).unwrap_err().kind;
    assert_eq!(kind(update(&[], false, None)), ErrorKind::InvalidInput);
    assert_eq!(
        kind(update(&["nothing-like-it"], false, None)),
        ErrorKind::NotFound
    );
    assert_eq!(
        kind(update(
            &["mock-weather-fx", "mock-textures"],
            false,
            Some("x")
        )),
        ErrorKind::InvalidInput
    );
    assert_eq!(
        kind(update(&["mock-weather-fx"], false, Some("nope"))),
        ErrorKind::NotFound
    );
    // `mockmr03v3` is the version the file already is.
    assert_eq!(
        kind(update(&["mock-weather-fx"], false, Some("mockmr03v3"))),
        ErrorKind::Conflict
    );

    // A file name, a slug and a project id all select the file.
    for key in ["mock-weather-fx-1.1.0.jar", "mock-weather-fx", "mockmr03"] {
        let Response::InstallPlan(one) = plan(update(&[key], false, None)).unwrap() else {
            panic!("expected a plan");
        };
        assert_eq!(one.items.len(), 1, "{key}");
    }
    let switch = update(&["mock-weather-fx"], false, Some("mockmr03v1"));
    let Response::InstallPlan(switch) = plan(switch).unwrap() else {
        panic!("expected a plan");
    };
    assert_eq!(switch.items[0].version_number, "2.1.0-beta.1");

    let one = update(&["mock-inventory-tweaks"], false, None);
    let response = catalog(&facade, CatalogCommand::Update(one)).unwrap();
    let (_, outcome) = run_task(&events, response);
    assert_eq!(outcome, TaskOutcome::Succeeded);
    let mods = files(&facade, "creative", ResourceKind::Mods);
    // The disabled mod stays disabled after its update.
    assert!(mods.contains(&("mock-inventory-tweaks-2.0.0.jar".into(), false)));
    let nothing = update(&["mock-library"], false, None);
    assert_eq!(
        catalog(&facade, CatalogCommand::Update(nothing))
            .unwrap_err()
            .kind,
        ErrorKind::Conflict
    );
}

#[test]
fn identify_reports_known_and_unknown_files() {
    let facade = mock();
    let identify = |world: Option<&str>| {
        catalog(
            &facade,
            CatalogCommand::IdentifyMods {
                instance: id("creative"),
                provider: Provider::Modrinth,
                world: world.map(Into::into),
            },
        )
    };
    let Response::Identified(report) = identify(None).unwrap() else {
        panic!("expected a report");
    };
    assert!(report.already_known >= 7);
    assert!(report.unknown.iter().any(|f| f.file == "local-tweak.jar"));
    assert_eq!(
        identify(Some("nowhere")).unwrap_err().kind,
        ErrorKind::NotFound
    );
}

fn pack_request(provider: Provider, project: &str) -> PackInstallRequest {
    PackInstallRequest {
        source: PackSource::Catalog {
            provider,
            project: project.into(),
            version: None,
        },
        name: None,
        group: None,
        min_stability: Default::default(),
    }
}

fn from_file(path: &str) -> PackInstallRequest {
    PackInstallRequest {
        source: PackSource::File { path: path.into() },
        ..pack_request(Provider::Modrinth, "x")
    }
}

fn install_pack(
    facade: &MockFacade,
    events: &EventStream,
    request: PackInstallRequest,
) -> (Vec<Event>, TaskOutcome) {
    let response = catalog(facade, CatalogCommand::InstallPack(request)).unwrap();
    run_task(events, response)
}

#[test]
fn a_modrinth_pack_installs_as_an_instance() {
    let facade = mock();
    let events = facade.subscribe();
    let request = pack_request(Provider::Modrinth, "mock-skyblock-lite");
    let (seen, outcome) = install_pack(&facade, &events, request);
    assert_eq!(outcome, TaskOutcome::Succeeded);
    assert!(seen.iter().any(|e| matches!(
        e,
        Event::Task(TaskEvent::Started(info)) if info.kind == TaskKind::InstallPack
    )));
    assert!(seen.iter().any(|e| matches!(
        e,
        Event::Instance(InstanceEvent::Added(i)) if i.name == "Mock Skyblock Lite"
    )));
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, Event::Log(l) if l.level == LogLevel::Warn))
    );
    assert_eq!(
        files(&facade, "mock-skyblock-lite", ResourceKind::Mods).len(),
        3
    );

    let mut named = pack_request(Provider::Modrinth, "mock-skyblock-lite");
    named.name = Some("  My pack ".into());
    named.group = Some("Packs".into());
    let (seen, _) = install_pack(&facade, &events, named);
    assert!(seen.iter().any(|e| matches!(
        e,
        Event::Instance(InstanceEvent::Added(i))
            if i.name == "My pack" && i.group.as_deref() == Some("Packs")
    )));
    let (_, outcome) = install_pack(&facade, &events, from_file("C:\\packs\\my-world.mrpack"));
    assert_eq!(outcome, TaskOutcome::Succeeded);
    assert!(instances(&facade).iter().any(|i| i.name == "my-world"));
}

#[test]
fn a_curseforge_pack_reports_the_blocked_file_and_failures_leave_nothing() {
    let facade = mock();
    let events = facade.subscribe();
    let request = pack_request(Provider::CurseForge, "mock-tech-pack");
    let (seen, outcome) = install_pack(&facade, &events, request);
    assert_eq!(outcome, TaskOutcome::Succeeded);
    let warnings: Vec<&str> = seen
        .iter()
        .filter_map(|e| match e {
            Event::Log(l) if l.level == LogLevel::Warn => Some(l.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(warnings[0].contains("download"));

    let before = instances(&facade).len();
    let (_, outcome) = install_pack(&facade, &events, from_file("/tmp/corrupt.mrpack"));
    assert!(matches!(outcome, TaskOutcome::Failed(e) if e.kind == ErrorKind::Integrity));
    assert_eq!(instances(&facade).len(), before);

    let (seen, _) = install_pack(&facade, &events, from_file("kitchen-blocked.zip"));
    assert!(
        seen.iter()
            .any(|e| matches!(e, Event::Log(l) if l.level == LogLevel::Warn))
    );
}

#[test]
fn pack_install_requests_are_checked() {
    let facade = mock();
    let kind = |request| {
        catalog(&facade, CatalogCommand::InstallPack(request))
            .unwrap_err()
            .kind
    };
    let (mr, cf) = (Provider::Modrinth, Provider::CurseForge);
    assert_eq!(
        kind(pack_request(mr, "mock-renderer")),
        ErrorKind::InvalidInput
    );
    assert_eq!(
        kind(pack_request(mr, "mock-tech-pack")),
        ErrorKind::NotFound
    );
    assert_eq!(kind(pack_request(cf, "nope")), ErrorKind::NotFound);
    let mut blank = pack_request(mr, "mock-skyblock-lite");
    blank.name = Some("  ".into());
    assert_eq!(kind(blank), ErrorKind::InvalidInput);
    assert_eq!(kind(from_file("pack.tar")), ErrorKind::InvalidInput);
    let mut missing = pack_request(mr, "mock-skyblock-lite");
    missing.source = PackSource::Catalog {
        provider: mr,
        project: "mock-skyblock-lite".into(),
        version: Some("nope".into()),
    };
    assert_eq!(kind(missing), ErrorKind::NotFound);
}

#[test]
fn a_pack_updates_keeping_the_users_changes() {
    let facade = mock();
    let events = facade.subscribe();
    let request = PackUpdateRequest::new(id("creative"));
    let check = |request: &PackUpdateRequest| {
        let Response::PackUpdate(info) =
            catalog(&facade, CatalogCommand::CheckPackUpdate(request.clone())).unwrap()
        else {
            panic!("expected an update info");
        };
        info
    };
    let plan = |request: &PackUpdateRequest| {
        let Response::PackChanges(changes) =
            catalog(&facade, CatalogCommand::PlanPackUpdate(request.clone())).unwrap()
        else {
            panic!("expected changes");
        };
        changes
    };
    let info = check(&request);
    assert_eq!(info.name, "Mock Adventure Pack");
    assert_eq!(info.current.number, "1.1.0");
    assert_eq!(
        info.latest.as_ref().map(|l| l.number.as_str()),
        Some("2.0.0")
    );

    let changes = plan(&request);
    assert_eq!(
        (changes.from.as_str(), changes.to.as_str()),
        ("1.1.0", "2.0.0")
    );
    let actions: Vec<PackAction> = changes.changes.iter().map(|c| c.action).collect();
    for wanted in [
        PackAction::Add,
        PackAction::Update,
        PackAction::Remove,
        PackAction::SaveNew,
        PackAction::KeepDeleted,
        PackAction::KeepModified,
    ] {
        assert!(actions.contains(&wanted), "{wanted:?}");
    }
    assert!(!changes.is_empty() && changes.download_bytes > 0 && changes.loader.is_some());

    let reset = plan(&PackUpdateRequest {
        reset: true,
        ..request.clone()
    });
    assert!(reset.reset);
    assert!(reset.changes.iter().any(|c| c.action == PackAction::Enable));
    assert!(
        !reset
            .changes
            .iter()
            .any(|c| c.action == PackAction::KeepDeleted)
    );

    let response = catalog(&facade, CatalogCommand::UpdatePack(request.clone())).unwrap();
    let (seen, outcome) = run_task(&events, response);
    assert_eq!(outcome, TaskOutcome::Succeeded);
    assert!(seen.iter().any(|e| matches!(
        e,
        Event::Task(TaskEvent::Started(info)) if info.kind == TaskKind::UpdatePack
    )));
    assert!(
        seen.iter()
            .any(|e| matches!(e, Event::Instance(InstanceEvent::Changed(_))))
    );
    let mods = files(&facade, "creative", ResourceKind::Mods);
    assert!(mods.iter().any(|(f, _)| f == "mock-renderer-2.0.0.jar"));
    assert!(
        mods.iter().any(|(f, _)| f == "local-tweak.jar"),
        "the user's file stays"
    );

    // Up to date now; an explicit older version is still offered.
    assert!(check(&request).latest.is_none());
    assert!(plan(&request).is_empty());
    assert_eq!(
        catalog(&facade, CatalogCommand::UpdatePack(request.clone()))
            .unwrap_err()
            .kind,
        ErrorKind::Conflict
    );
    let older = PackUpdateRequest {
        version: Some("mockmr05v3".into()),
        ..request
    };
    assert_eq!(
        check(&older).latest.map(|l| l.number),
        Some("1.1.0".to_owned())
    );
}

#[test]
fn pack_updates_need_a_pack_and_handle_files_and_blocked_files() {
    let facade = mock();
    let events = facade.subscribe();
    let check = |instance: &str| {
        catalog(
            &facade,
            CatalogCommand::CheckPackUpdate(PackUpdateRequest::new(id(instance))),
        )
    };
    assert_eq!(check("survival").unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(check("nope").unwrap_err().kind, ErrorKind::NotFound);

    // A pack installed from a file is updated with a file.
    install_pack(&facade, &events, from_file("my-world.mrpack"));
    let Response::PackUpdate(info) = check("my-world").unwrap() else {
        panic!("expected an update info");
    };
    assert!(info.from_file && info.latest.is_none());
    let plan = |file: Option<&str>| {
        catalog(
            &facade,
            CatalogCommand::PlanPackUpdate(PackUpdateRequest {
                file: file.map(Into::into),
                ..PackUpdateRequest::new(id("my-world"))
            }),
        )
    };
    assert_eq!(plan(None).unwrap_err().kind, ErrorKind::InvalidInput);
    assert!(plan(Some("new.mrpack")).is_ok());

    // An old version of the blocked CurseForge pack warns about the file in every answer.
    let mut old = pack_request(Provider::CurseForge, "mock-tech-pack");
    old.source = PackSource::Catalog {
        provider: Provider::CurseForge,
        project: "mock-tech-pack".into(),
        version: Some("8100152".into()),
    };
    install_pack(&facade, &events, old);
    let Response::PackUpdate(info) = check("mock-tech-pack").unwrap() else {
        panic!("expected an update info");
    };
    assert!(info.latest.is_some());
    assert!(info.warnings[0].contains("by hand"));
    let changes = catalog(
        &facade,
        CatalogCommand::PlanPackUpdate(PackUpdateRequest::new(id("mock-tech-pack"))),
    )
    .unwrap();
    let Response::PackChanges(changes) = changes else {
        panic!("expected changes");
    };
    assert_eq!(changes.warnings.len(), 1);
    assert!(
        changes
            .loader
            .as_ref()
            .is_some_and(|l| l.from.starts_with("forge"))
    );
}

#[test]
fn an_instance_exports_as_a_pack() {
    let facade = mock();
    let export = |out: &str, include: Vec<ExportInclude>| {
        catalog(
            &facade,
            CatalogCommand::ExportPack(PackExportRequest {
                include,
                ..PackExportRequest::new(id("creative"), out.into())
            }),
        )
    };
    let Response::PackExported(done) =
        export("creative.mrpack", ExportInclude::DEFAULT.to_vec()).unwrap()
    else {
        panic!("expected an export");
    };
    assert_eq!((done.linked, done.disabled, done.bundled), (8, 1, 14));
    assert_eq!(done.unlisted, ["local-tweak.jar", "hand-made-pack"]);
    assert!(done.size > 0);
    assert_eq!(done.warnings.len(), 1);

    let Response::PackExported(bare) = export("x.zip", vec![]).unwrap() else {
        panic!("expected an export");
    };
    assert_eq!((bare.linked, bare.bundled), (6, 1));
    assert!(bare.warnings.iter().any(|w| w.contains(".mrpack")));
    assert_eq!(
        export(" ", vec![]).unwrap_err().kind,
        ErrorKind::InvalidInput
    );
}

#[test]
fn a_pack_install_can_be_cancelled_and_leaves_no_instance() {
    let facade = MockFacade::new(MockOptions {
        step_delay: std::time::Duration::from_millis(20),
        ..MockOptions::instant()
    });
    let events = facade.subscribe();
    let before = instances(&facade).len();
    let response = catalog(
        &facade,
        CatalogCommand::InstallPack(pack_request(Provider::Modrinth, "mock-skyblock-lite")),
    )
    .unwrap();
    let Response::Task(task) = &response else {
        panic!("expected a task");
    };
    call(
        &facade,
        Command::Tasks(vl_core::task::TaskCommand::Cancel { id: *task }),
    )
    .unwrap();
    let (_, outcome) = run_task(&events, response);
    assert_eq!(outcome, TaskOutcome::Cancelled);
    assert_eq!(instances(&facade).len(), before);
}

#[test]
fn the_api_key_is_set_checked_and_removed() {
    let facade = MockFacade::new(MockOptions::instant());
    let check = |provider| catalog(&facade, CatalogCommand::CheckApiKey { provider });
    let Response::ApiKey(missing) = check(Provider::CurseForge).unwrap() else {
        panic!("expected a key status");
    };
    assert_eq!(missing.state, ApiKeyState::Missing);
    assert!(missing.source.is_none() && !missing.in_keyring);
    assert_eq!(
        check(Provider::Modrinth).unwrap_err().kind,
        ErrorKind::InvalidInput
    );

    let set = |key: &str| {
        catalog(
            &facade,
            CatalogCommand::SetApiKey {
                provider: Provider::CurseForge,
                key: Secret::new(key),
            },
        )
    };
    assert_eq!(set(" ").unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(set("synthetic-key"), Ok(Response::Done));
    let Response::ApiKey(valid) = check(Provider::CurseForge).unwrap() else {
        panic!("expected a key status");
    };
    assert_eq!(valid.state, ApiKeyState::Valid);
    assert_eq!(valid.source, Some(ApiKeyOrigin::Keyring));
    assert!(
        search(
            &facade,
            SearchQuery {
                provider: Provider::CurseForge,
                ..SearchQuery::default()
            }
        )
        .is_ok()
    );

    let remove = |provider| catalog(&facade, CatalogCommand::RemoveApiKey { provider });
    assert_eq!(
        remove(Provider::Modrinth).unwrap_err().kind,
        ErrorKind::InvalidInput
    );
    assert_eq!(remove(Provider::CurseForge), Ok(Response::Done));
    let Response::ApiKey(gone) = check(Provider::CurseForge).unwrap() else {
        panic!("expected a key status");
    };
    assert_eq!(gone.state, ApiKeyState::Missing);
    let set_modrinth = CatalogCommand::SetApiKey {
        provider: Provider::Modrinth,
        key: Secret::new("x"),
    };
    assert_eq!(
        catalog(&facade, set_modrinth).unwrap_err().kind,
        ErrorKind::InvalidInput
    );
}

#[test]
fn an_update_check_covers_both_platforms_or_one() {
    let facade = mock();
    let check = |provider| {
        let request = UpdateCheckRequest {
            provider,
            include_pack_files: true,
            ..UpdateCheckRequest::new(id("creative"))
        };
        let Response::UpdateReport(report) =
            catalog(&facade, CatalogCommand::CheckUpdates(request)).unwrap()
        else {
            panic!("expected a report");
        };
        report
    };
    let both = check(None);
    assert!(
        both.updates
            .iter()
            .any(|u| u.provider == Provider::Modrinth)
    );
    assert!(
        both.updates
            .iter()
            .any(|u| u.provider == Provider::CurseForge)
    );
    assert!(both.warnings.is_empty());
    let only = check(Some(Provider::CurseForge));
    assert!(
        only.updates
            .iter()
            .all(|u| u.provider == Provider::CurseForge)
    );
    assert!(!only.updates.is_empty());

    // Without a key the other platform is still checked and the gap is reported.
    let no_key = MockFacade::new(MockOptions::instant());
    let request = UpdateCheckRequest::new(id("creative"));
    let Response::UpdateReport(partial) =
        catalog(&no_key, CatalogCommand::CheckUpdates(request)).unwrap()
    else {
        panic!("expected a report");
    };
    assert!(
        partial
            .updates
            .iter()
            .all(|u| u.provider == Provider::Modrinth)
    );
    assert_eq!(partial.warnings.len(), 1);
}
