//! `catalog search | project | versions | lookup | install | key`.

use std::io::{BufRead, IsTerminal};
use std::time::Duration;

use vl_core::catalog::{
    CatalogCommand, HashAlgorithm, HashMatch, Project, ProjectType, ProjectVersion, SearchPage,
    SearchQuery, SearchSort, VersionFilter,
};
use vl_core::install::{InstallPlan, InstallRequest, MinStability, PlanAction, PlanItem, PlanRole};
use vl_core::task::TaskOutcome;
use vl_core::{Command, Event, Facade, InstanceId, Response};

use super::{CliError, Out, USAGE, call, unexpected};
use crate::progress::{ProgressPrinter, human_bytes};

/// How long `vl-cli` waits for an event before it waits again.
const POLL: Duration = Duration::from_millis(250);

pub(crate) fn run(facade: &dyn Facade, args: &[&str], out: Out<'_>) -> Result<(), CliError> {
    match args {
        ["install", project, flags @ ..] => install(facade, project, flags, out),
        ["search", query, flags @ ..] => search(facade, query, flags, out),
        ["project", id] => project(facade, id, out),
        ["versions", id, flags @ ..] => versions(facade, id, flags, out),
        ["lookup", hashes @ ..] if !hashes.is_empty() => lookup(facade, hashes, out),
        ["key", rest @ ..] => super::api_key::run(facade, rest, out),
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

/// `catalog install <slug|id> --instance <id> [--world <name>] [--version <id>] [--channel <c>]
/// [--yes]`: prints
/// the plan, asks (or takes `--yes`), then follows the install task.
fn install(
    facade: &dyn Facade,
    project: &str,
    flags: &[&str],
    out: Out<'_>,
) -> Result<(), CliError> {
    let mut instance = None;
    let mut version = None;
    let mut world = None;
    let mut min_stability = MinStability::default();
    let mut yes = false;
    let mut it = flags.iter();
    while let Some(flag) = it.next() {
        if *flag == "--yes" {
            yes = true;
            continue;
        }
        let value = it.next().ok_or(CliError::from(USAGE))?;
        match *flag {
            "--instance" => instance = Some((*value).to_owned()),
            "--version" => version = Some((*value).to_owned()),
            "--world" => world = Some((*value).to_owned()),
            "--channel" => {
                min_stability = MinStability::parse(value)
                    .ok_or_else(|| CliError::from("channel is release, beta or alpha"))?;
            }
            _ => return Err(USAGE.into()),
        }
    }
    let instance = instance
        .and_then(|i| InstanceId::new(&i))
        .ok_or_else(|| CliError::from("--instance <id> is required"))?;
    let request = InstallRequest {
        instance: instance.clone(),
        provider: Default::default(),
        project: project.to_owned(),
        version,
        min_stability,
        world,
    };
    let planned = call(
        facade,
        Command::Catalog(CatalogCommand::PlanInstall(request.clone())),
    )?;
    let plan = match planned {
        Response::InstallPlan(plan) => plan,
        other => return Err(unexpected(&other)),
    };
    print_plan(&instance, &plan, out)?;
    if plan.downloads().next().is_none() {
        writeln!(out, "nothing to install")?;
        return Ok(());
    }
    if !yes && !confirmed(out)? {
        writeln!(out, "not installed; add --yes to install")?;
        return Ok(());
    }
    run_task(
        facade,
        Command::Catalog(CatalogCommand::Install(request)),
        out,
    )
}

/// Starts the task a command returns and follows it to its end.
pub(crate) fn run_task(
    facade: &dyn Facade,
    command: Command,
    out: Out<'_>,
) -> Result<(), CliError> {
    // Subscribe before the command so no event is missed.
    let events = facade.subscribe();
    let task = match call(facade, command)? {
        Response::Task(task) => task,
        other => return Err(unexpected(&other)),
    };
    let mut printer = ProgressPrinter::new(task, "install");
    loop {
        let Some(event) = events.next_timeout(POLL) else {
            continue;
        };
        match event {
            Event::Task(task_event) => match printer.handle(&task_event, out)? {
                Some(TaskOutcome::Succeeded) => {
                    writeln!(out, "[install] done")?;
                    return Ok(());
                }
                Some(TaskOutcome::Failed(e)) => return Err(e.into()),
                Some(_) => return Err("install cancelled".into()),
                None => {}
            },
            Event::Lagged { missed } => writeln!(out, "[vl-cli] {missed} events skipped")?,
            _ => {}
        }
    }
}

/// Asks on a terminal; anything else is a plan only.
pub(crate) fn confirmed(out: Out<'_>) -> Result<bool, CliError> {
    confirmed_with(out, "install?")
}

/// [`confirmed`] with another question.
pub(crate) fn confirmed_with(out: Out<'_>, question: &str) -> Result<bool, CliError> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return Ok(false);
    }
    write!(out, "{question} [y/N] ")?;
    out.flush()?;
    let mut answer = String::new();
    stdin.lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

pub(crate) fn print_plan(
    instance: &InstanceId,
    plan: &InstallPlan,
    out: Out<'_>,
) -> Result<(), CliError> {
    writeln!(
        out,
        "plan for {instance} (Minecraft {}, {}):",
        plan.game_version,
        plan.loader.as_deref().unwrap_or("no loader")
    )?;
    for item in &plan.items {
        print_item(item, out)?;
    }
    for s in &plan.satisfied {
        writeln!(
            out,
            "  have      {} {} ({}, matched by {:?})",
            s.title,
            s.version.as_deref().unwrap_or(""),
            s.file,
            s.matched_by
        )?;
    }
    for r in &plan.recommended {
        writeln!(
            out,
            "  optional  {} (suggested by {}); not installed",
            r.slug.as_deref().unwrap_or(&r.project_id),
            r.suggested_by
        )?;
    }
    for w in &plan.warnings {
        writeln!(out, "  warning   {w}")?;
    }
    let count = plan.downloads().count();
    writeln!(
        out,
        "{count} file(s) to download, {}",
        human_bytes(plan.download_bytes())
    )?;
    Ok(())
}

fn print_item(item: &PlanItem, out: Out<'_>) -> Result<(), CliError> {
    let verb = match &item.action {
        PlanAction::Install => "install",
        PlanAction::Present { .. } => "have",
        PlanAction::Replace { .. } => "replace",
        _ => "change",
    };
    let channel = format!("{:?}", item.channel).to_lowercase();
    write!(
        out,
        "  {verb:<9} {} {} [{channel}] {} -> {}/ ({})",
        item.title,
        item.version_number,
        item.file.filename,
        item.kind.dir(),
        human_bytes(item.file.size)
    )?;
    match &item.action {
        PlanAction::Replace { file, old_version } => write!(
            out,
            "; replaces {file}{}",
            old_version
                .as_deref()
                .map(|v| format!(" ({v})"))
                .unwrap_or_default()
        )?,
        PlanAction::Present { file } => write!(out, "; already there as {file}")?,
        _ => {}
    }
    if let PlanRole::Dependency { required_by } = &item.role {
        write!(out, "; needed by {required_by}")?;
    }
    writeln!(out)?;
    Ok(())
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
        assert!(out.contains("5 of 5 results"), "{out}");
    }

    #[test]
    fn shows_a_project_and_its_versions() {
        let out = run_mock(&["project", "mock-renderer"]).unwrap();
        assert!(out.contains("Mock Renderer"), "{out}");
        let out = run_mock(&["versions", "mock-renderer", "--version", "26.3"]).unwrap();
        assert!(out.contains("2.0.0"), "{out}");
        assert!(out.contains("Required: mockmr00"), "{out}");
        let err = run_mock(&["project", "missing"]).unwrap_err();
        assert!(err.to_string().contains("NotFound"), "{err}");
    }

    #[test]
    fn looks_up_hashes() {
        let out = run_mock(&["lookup", &"a".repeat(40)]).unwrap();
        assert!(out.contains("0 of 1 files known"), "{out}");
    }

    #[test]
    fn install_checks_its_arguments() {
        assert!(
            run_mock(&["install", "x"]).is_err(),
            "--instance is required"
        );
        assert!(run_mock(&["install", "x", "--instance"]).is_err());
        assert!(run_mock(&["install", "x", "--instance", "a", "--channel", "nightly"]).is_err());
        assert!(run_mock(&["install", "x", "--instance", "a", "--nope", "1"]).is_err());
    }

    #[test]
    fn prints_a_plan() {
        use vl_core::catalog::{ProjectType, Provider, ReleaseChannel, VersionFile};
        use vl_core::install::{MatchedBy, Recommendation, Satisfied};
        use vl_core::resources::ResourceKind;

        let item = |title: &str, role, action| PlanItem {
            provider: Provider::Modrinth,
            project_id: title.to_lowercase(),
            slug: title.to_lowercase(),
            title: title.to_owned(),
            project_type: ProjectType::Mod,
            kind: ResourceKind::Mods,
            role,
            version_id: "v".into(),
            version_number: "1.0".into(),
            channel: ReleaseChannel::Release,
            file: VersionFile {
                filename: format!("{}.jar", title.to_lowercase()),
                url: "https://example.invalid/f".into(),
                size: 2048,
                sha1: None,
                sha512: None,
                primary: true,
            },
            action,
        };
        let plan = InstallPlan {
            game_version: "1.21.4".into(),
            loader: Some("fabric".into()),
            items: vec![
                item(
                    "Lib",
                    PlanRole::Dependency {
                        required_by: "Sodium".into(),
                    },
                    PlanAction::Install,
                ),
                item(
                    "Sodium",
                    PlanRole::Requested,
                    PlanAction::Replace {
                        file: "old.jar".into(),
                        old_version: Some("0.9".into()),
                    },
                ),
            ],
            satisfied: vec![Satisfied {
                project_id: "api".into(),
                slug: "api".into(),
                title: "Api".into(),
                file: "api.jar".into(),
                version: Some("3".into()),
                matched_by: MatchedBy::Hash,
            }],
            recommended: vec![Recommendation {
                project_id: "x".into(),
                slug: Some("extra".into()),
                title: None,
                suggested_by: "Sodium".into(),
            }],
            warnings: vec!["careful".into()],
        };
        let mut out = Vec::new();
        let id = InstanceId::new("main").unwrap();
        print_plan(&id, &plan, &mut out).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.contains("plan for main (Minecraft 1.21.4, fabric)"),
            "{out}"
        );
        assert!(
            out.contains("install   Lib 1.0 [release] lib.jar -> mods/"),
            "{out}"
        );
        assert!(out.contains("needed by Sodium"), "{out}");
        assert!(out.contains("replaces old.jar (0.9)"), "{out}");
        assert!(out.contains("have      Api 3 (api.jar"), "{out}");
        assert!(
            out.contains("optional  extra (suggested by Sodium); not installed"),
            "{out}"
        );
        assert!(out.contains("warning   careful"), "{out}");
        assert!(out.contains("2 file(s) to download, 4.0 KiB"), "{out}");
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
