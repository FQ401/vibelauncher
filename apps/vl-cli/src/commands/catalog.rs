//! `catalog search | project | versions | lookup`.

use vl_core::catalog::{
    CatalogCommand, HashAlgorithm, HashMatch, Project, ProjectType, ProjectVersion, SearchPage,
    SearchQuery, SearchSort, VersionFilter,
};
use vl_core::{Command, Facade, Response};

use super::{CliError, Out, USAGE, call, unexpected};

pub(crate) fn run(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    match args {
        ["search", query, flags @ ..] => search(facade, query, flags, out),
        ["project", id] => project(facade, id, out),
        ["versions", id, flags @ ..] => versions(facade, id, flags, out),
        ["lookup", hashes @ ..] if !hashes.is_empty() => lookup(facade, hashes, out),
        _ => Err(USAGE.into()),
    }
}

/// Reads `--flag value` pairs; `handle` returns false for a flag it does not know.
fn flag_pairs(
    flags: &[&str],
    mut handle: impl FnMut(&str, &str) -> Result<bool, CliError>,
) -> Result<(), CliError> {
    let mut it = flags.iter();
    while let Some(flag) = it.next() {
        let value = it.next().ok_or(CliError::from(USAGE))?;
        if !handle(flag, value)? {
            return Err(USAGE.into());
        }
    }
    Ok(())
}

fn number(value: &str, what: &str) -> Result<u32, CliError> {
    value
        .parse()
        .map_err(|_| CliError::from(format!("{what} must be a number")))
}

impl From<String> for CliError {
    fn from(e: String) -> Self {
        Self(e)
    }
}

fn search(facade: &dyn Facade, text: &str, flags: &[&str], out: Out<'_>) -> Result<(), CliError> {
    let mut query = SearchQuery::text(text);
    flag_pairs(flags, |flag, value| {
        match flag {
            "--type" => {
                query.project_type = Some(ProjectType::parse(value).ok_or_else(|| {
                    CliError::from("type is mod, modpack, resourcepack, shader or datapack")
                })?);
            }
            "--version" => query.game_version = Some(value.to_owned()),
            "--loader" => query.loader = Some(value.to_owned()),
            "--category" => query.categories.push(value.to_owned()),
            "--sort" => {
                query.sort = SearchSort::parse(value).ok_or_else(|| {
                    CliError::from("sort is relevance, downloads, follows, newest or updated")
                })?;
            }
            "--offset" => query.offset = number(value, "offset")?,
            "--limit" => query.limit = number(value, "limit")?,
            _ => return Ok(false),
        }
        Ok(true)
    })?;
    match call(facade, Command::Catalog(CatalogCommand::Search(query)))? {
        Response::CatalogSearch(page) => print_search(&page, out),
        other => Err(unexpected(&other)),
    }
}

fn print_search(page: &SearchPage, out: Out<'_>) -> Result<(), CliError> {
    for hit in &page.hits {
        writeln!(
            out,
            "{:<28} {:<12} {:>10} downloads  {}",
            hit.slug,
            hit.project_type.name(),
            hit.downloads,
            hit.title
        )?;
        if !hit.description.is_empty() {
            writeln!(out, "    {}", one_line(&hit.description))?;
        }
    }
    writeln!(
        out,
        "{} of {} results (from {})",
        page.hits.len(),
        page.total_hits,
        page.offset
    )?;
    Ok(())
}

fn project(facade: &dyn Facade, id: &str, out: Out<'_>) -> Result<(), CliError> {
    let command = Command::Catalog(CatalogCommand::Project {
        id: (*id).to_owned(),
    });
    match call(facade, command)? {
        Response::CatalogProject(project) => print_project(&project, out),
        other => Err(unexpected(&other)),
    }
}

fn print_project(p: &Project, out: Out<'_>) -> Result<(), CliError> {
    writeln!(out, "{} ({})", p.title, p.slug)?;
    writeln!(out, "  id:          {}", p.id)?;
    writeln!(out, "  type:        {}", p.project_type.name())?;
    writeln!(out, "  about:       {}", one_line(&p.description))?;
    writeln!(
        out,
        "  downloads:   {}  followers: {}",
        p.downloads, p.followers
    )?;
    writeln!(
        out,
        "  client/server: {:?} / {:?}",
        p.client_side, p.server_side
    )?;
    writeln!(out, "  loaders:     {}", p.loaders.join(", "))?;
    writeln!(out, "  categories:  {}", p.categories.join(", "))?;
    writeln!(out, "  game versions: {}", summarize(&p.game_versions))?;
    if let Some(license) = &p.license {
        writeln!(out, "  license:     {license}")?;
    }
    if let Some(url) = &p.source_url {
        writeln!(out, "  source:      {url}")?;
    }
    writeln!(out, "  versions:    {}", p.version_ids.len())?;
    Ok(())
}

fn versions(facade: &dyn Facade, id: &str, flags: &[&str], out: Out<'_>) -> Result<(), CliError> {
    let mut filter = VersionFilter::default();
    flag_pairs(flags, |flag, value| {
        match flag {
            "--version" => filter.game_versions.push(value.to_owned()),
            "--loader" => filter.loaders.push(value.to_owned()),
            _ => return Ok(false),
        }
        Ok(true)
    })?;
    let command = Command::Catalog(CatalogCommand::Versions {
        project: (*id).to_owned(),
        filter,
    });
    match call(facade, command)? {
        Response::CatalogVersions(versions) => {
            for v in &versions {
                print_version(v, out)?;
            }
            writeln!(out, "{} versions", versions.len())?;
            Ok(())
        }
        other => Err(unexpected(&other)),
    }
}

fn print_version(v: &ProjectVersion, out: Out<'_>) -> Result<(), CliError> {
    writeln!(
        out,
        "{:<20} {:<8} {:<16} {}",
        v.version_number,
        format!("{:?}", v.channel).to_lowercase(),
        v.loaders.join(","),
        summarize(&v.game_versions)
    )?;
    if let Some(file) = v.primary_file() {
        writeln!(out, "    {} ({} bytes)", file.filename, file.size)?;
    }
    for d in &v.dependencies {
        let target = d
            .project_id
            .as_deref()
            .or(d.file_name.as_deref())
            .or(d.version_id.as_deref())
            .unwrap_or("?");
        writeln!(out, "    {:?}: {target}", d.kind)?;
    }
    Ok(())
}

/// Finds which Modrinth versions own files with the given SHA-1 hashes.
fn lookup(facade: &dyn Facade, hashes: &[&str], out: Out<'_>) -> Result<(), CliError> {
    let command = Command::Catalog(CatalogCommand::VersionsByHashes {
        algorithm: HashAlgorithm::Sha1,
        hashes: hashes.iter().map(|h| (*h).to_owned()).collect(),
    });
    match call(facade, command)? {
        Response::CatalogHashMatches(matches) => {
            for HashMatch { hash, version } in &matches {
                writeln!(
                    out,
                    "{hash}  project {}  version {} ({})",
                    version.project_id, version.version_number, version.id
                )?;
            }
            writeln!(out, "{} of {} files known", matches.len(), hashes.len())?;
            Ok(())
        }
        other => Err(unexpected(&other)),
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The first versions of a long list, then a count.
fn summarize(list: &[String]) -> String {
    const SHOWN: usize = 6;
    if list.len() <= SHOWN {
        list.join(", ")
    } else {
        format!("{}, ... ({} in all)", list[..SHOWN].join(", "), list.len())
    }
}

#[cfg(test)]
mod tests {
    use vl_mock::{MockFacade, MockOptions};

    use super::*;

    fn run_mock(args: &[&str]) -> Result<String, CliError> {
        let facade = MockFacade::new(MockOptions::instant());
        let mut out = Vec::new();
        run(&facade, args, &mut out)?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    #[test]
    fn searches_with_filters() {
        let out = run_mock(&["search", "mock", "--type", "mod", "--loader", "fabric"]).unwrap();
        assert!(out.contains("mock-renderer"), "{out}");
        assert!(!out.contains("mock-textures"), "{out}");
        assert!(out.contains("1 of 1 results"), "{out}");
    }

    #[test]
    fn shows_a_project_and_its_versions() {
        let out = run_mock(&["project", "mock-renderer"]).unwrap();
        assert!(out.contains("Mock Renderer"), "{out}");
        let out = run_mock(&["versions", "mock-renderer", "--version", "26.3"]).unwrap();
        assert!(out.contains("1.0.0"), "{out}");
        assert!(out.contains("Required: mock-library"), "{out}");
        let err = run_mock(&["project", "missing"]).unwrap_err();
        assert!(err.to_string().contains("NotFound"), "{err}");
    }

    #[test]
    fn looks_up_hashes() {
        let out = run_mock(&["lookup", &"a".repeat(40)]).unwrap();
        assert!(out.contains("0 of 1 files known"), "{out}");
    }

    #[test]
    fn checks_its_arguments() {
        assert!(run_mock(&["search"]).is_err());
        assert!(run_mock(&["search", "x", "--type", "plugin"]).is_err());
        assert!(run_mock(&["search", "x", "--limit", "many"]).is_err());
        assert!(run_mock(&["search", "x", "--loader"]).is_err());
        assert!(run_mock(&["search", "x", "--unknown", "1"]).is_err());
        assert!(run_mock(&["search", "x", "--limit", "0"]).is_err());
        assert!(run_mock(&["lookup"]).is_err());
    }
}
