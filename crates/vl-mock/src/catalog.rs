//! Synthetic catalog answers: two invented projects, searched by substring.

use vl_core::catalog::{
    CatalogCommand, Dependency, DependencyKind, Project, ProjectType, ProjectVersion,
    ReleaseChannel, SearchHit, SearchPage, SideSupport, VersionFile,
};
use vl_core::{Error, Response, Result};

/// `(slug, title, type)` of the invented projects.
const PROJECTS: [(&str, &str, ProjectType); 2] = [
    ("mock-renderer", "Mock Renderer", ProjectType::Mod),
    ("mock-textures", "Mock Textures", ProjectType::ResourcePack),
];

fn hit(slug: &str, title: &str, kind: ProjectType) -> SearchHit {
    SearchHit {
        project_id: format!("mock-{slug}"),
        slug: slug.into(),
        title: title.into(),
        description: format!("{title}: synthetic catalog entry"),
        project_type: kind,
        author: "mock-author".into(),
        categories: vec!["fabric".into(), "utility".into()],
        game_versions: vec!["26.3".into()],
        downloads: 1234,
        follows: 56,
        icon_url: None,
        date_modified: "2026-01-01T00:00:00Z".into(),
    }
}

fn project(slug: &str, title: &str, kind: ProjectType) -> Project {
    Project {
        id: format!("mock-{slug}"),
        slug: slug.into(),
        title: title.into(),
        description: format!("{title}: synthetic catalog entry"),
        body: "A synthetic project for tests and demos.".into(),
        project_type: kind,
        categories: vec!["utility".into()],
        loaders: vec!["fabric".into()],
        game_versions: vec!["26.3".into()],
        downloads: 1234,
        followers: 56,
        icon_url: None,
        license: Some("MIT".into()),
        source_url: None,
        issues_url: None,
        client_side: SideSupport::Required,
        server_side: SideSupport::Optional,
        updated: "2026-01-01T00:00:00Z".into(),
        version_ids: vec![format!("mock-{slug}-1")],
    }
}

fn version(slug: &str) -> ProjectVersion {
    ProjectVersion {
        id: format!("mock-{slug}-1"),
        project_id: format!("mock-{slug}"),
        name: "1.0.0".into(),
        version_number: "1.0.0".into(),
        channel: ReleaseChannel::Release,
        game_versions: vec!["26.3".into()],
        loaders: vec!["fabric".into()],
        date_published: "2026-01-01T00:00:00Z".into(),
        downloads: 1234,
        files: vec![VersionFile {
            filename: format!("{slug}-1.0.0.jar"),
            url: format!("https://cdn.example.invalid/{slug}-1.0.0.jar"),
            size: 1024,
            sha1: None,
            sha512: None,
            primary: true,
        }],
        dependencies: vec![Dependency {
            project_id: Some("mock-library".into()),
            version_id: None,
            file_name: None,
            kind: DependencyKind::Required,
        }],
    }
}

fn find(id: &str) -> Result<&'static (&'static str, &'static str, ProjectType)> {
    PROJECTS
        .iter()
        .find(|(slug, ..)| *slug == id || format!("mock-{slug}") == id)
        .ok_or_else(|| Error::not_found(format!("project {id}")))
}

pub(crate) fn handle(command: CatalogCommand) -> Result<Response> {
    match command {
        CatalogCommand::Search(query) => {
            query.validate()?;
            let needle = query.query.to_lowercase();
            let hits: Vec<_> = PROJECTS
                .iter()
                .filter(|(slug, title, kind)| {
                    (slug.contains(&needle) || title.to_lowercase().contains(&needle))
                        && query.project_type.is_none_or(|t| t == *kind)
                })
                .map(|(slug, title, kind)| hit(slug, title, *kind))
                .collect();
            Ok(Response::CatalogSearch(SearchPage {
                total_hits: hits.len() as u64,
                hits,
                offset: query.offset,
                limit: query.limit,
            }))
        }
        CatalogCommand::Project { id } => {
            let (slug, title, kind) = find(&id)?;
            Ok(Response::CatalogProject(project(slug, title, *kind)))
        }
        CatalogCommand::Versions { project, .. } => {
            let (slug, ..) = find(&project)?;
            Ok(Response::CatalogVersions(vec![version(slug)]))
        }
        CatalogCommand::VersionsByHashes { .. } => Ok(Response::CatalogHashMatches(Vec::new())),
        other => Err(Error::unsupported(format!(
            "vl-mock does not simulate {other:?}"
        ))),
    }
}
