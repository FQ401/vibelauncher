//! Project tasks, run as `cargo xtask <task>`:
//! - `ci` — every check a PR must pass: layers, format, clippy, tests, wasm build of the core layer and the shared UI;
//! - `ci lint` / `ci test` — the same checks split in two halves so CI can run them in parallel;
//! - `version` — prints the launcher version (`[workspace.package] version`, the only place it lives);
//! - `version set X.Y.Z[-pre]` — sets it;
//! - `package portable [--target <triple>]` — the portable Windows archive in `target/dist`;
//! - `package tar|appimage|deb|rpm|linux [--target <triple>]` — the Linux packages in `target/dist`
//!   (`linux`: all four);
//! - `package macos` — the universal macOS disk image in `target/dist` (on macOS);
//! - `sources` — every dependency in `Cargo.lock` comes from crates.io or the workspace;
//! - `release manifest <dir> --repo <owner/name> --date <YYYY-MM-DD> [--minimum-version <v>]` —
//!   `SHA256SUMS.txt` and the self-update manifest `latest.json` for the packages in `<dir>`;
//! - `release sign <dir>` — `latest.json.minisig` with the key in `VL_UPDATE_SECRET_KEY`.
#![allow(clippy::print_stdout, clippy::print_stderr)] // a command-line tool reports to the terminal

mod layers;
mod linux;
mod macos;
mod package;
mod release;
mod sources;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const USAGE: &str = "usage: cargo xtask ci [lint|test] | cargo xtask version [set X.Y.Z[-pre]] \
                     | cargo xtask package portable|tar|appimage|deb|rpm|linux [--target <triple>] \
                     | cargo xtask package macos | cargo xtask sources \
                     | cargo xtask release manifest <dir> --repo <owner/name> --date <YYYY-MM-DD> \
                     [--minimum-version <X.Y.Z>] | cargo xtask release sign <dir>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["ci"] => ci(true, true),
        ["ci", "lint"] => ci(true, false),
        ["ci", "test"] => ci(false, true),
        ["version"] => read_version().map(|v| println!("{v}")),
        ["version", "set", v] => set_version(v),
        ["package", "macos"] => macos::package(),
        ["sources"] => sources::check(),
        ["release", "sign", dir] => release::sign(Path::new(dir)),
        ["release", "manifest", dir, rest @ ..] => {
            let Some(options) = release_options(rest) else {
                return usage();
            };
            release::manifest(Path::new(dir), &options)
        }
        ["package", kind, rest @ ..] => {
            let target = match rest {
                [] => None,
                ["--target", t] => Some(*t),
                _ => return usage(),
            };
            match (*kind, linux::formats(kind)) {
                ("portable", _) => package::portable(target).map(|_| ()),
                (_, Some(formats)) => linux::package(&formats, target),
                (_, None) => return usage(),
            }
        }
        _ => return usage(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `--repo <owner/name> --date <YYYY-MM-DD> [--minimum-version <X.Y.Z>]` in any order; the
/// minimum version defaults to `0.0.0` (every version may update itself).
fn release_options<'a>(args: &[&'a str]) -> Option<release::ManifestOptions<'a>> {
    let (mut repo, mut date, mut minimum_version) = (None, None, "0.0.0");
    for pair in args.chunks(2) {
        match pair {
            ["--repo", v] => repo = Some(*v),
            ["--date", v] => date = Some(*v),
            ["--minimum-version", v] => minimum_version = v,
            _ => return None,
        }
    }
    Some(release::ManifestOptions {
        repo: repo?,
        date: date?,
        minimum_version,
    })
}

fn usage() -> ExitCode {
    eprintln!("xtask: {USAGE}");
    ExitCode::FAILURE
}

/// The workspace root (the parent of this crate).
fn root() -> PathBuf {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    here.parent().unwrap_or(here).to_path_buf()
}

/// Runs the checks in order and stops at the first failure.
fn ci(lint: bool, test: bool) -> Result<(), String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    if test && !lint {
        run("tests", &cargo, &["test", "--workspace"])?;
        println!("xtask ci: all checks passed");
        return Ok(());
    }
    let core = check_layers(&cargo)?;
    sources::check()?;
    let steps: [(&str, &str, &[&str]); 2] = [
        ("format", &cargo, &["fmt", "--all", "--check"]),
        (
            "clippy",
            &cargo,
            &[
                "clippy",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
        ),
    ];
    for (name, program, args) in steps {
        run(name, program, args)?;
    }
    if test {
        run("tests", &cargo, &["test", "--workspace"])?;
    }
    // The core layer is pure and the shared UI has no native dependencies: both must build for
    // the browser.
    let mut wasm = vec!["check", "--target", "wasm32-unknown-unknown"];
    for package in core.iter().map(String::as_str).chain(["vl-ui-egui"]) {
        wasm.extend(["-p", package]);
    }
    run("wasm", &cargo, &wasm)?;
    println!("xtask ci: all checks passed");
    Ok(())
}

/// Checks the layer rules; returns the packages of the core layer.
fn check_layers(cargo: &str) -> Result<Vec<String>, String> {
    println!("== layers: {cargo} metadata --no-deps");
    let output = Command::new(cargo)
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(root())
        .output()
        .map_err(|e| format!("layers: cannot run {cargo}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "layers: cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let packages = layers::packages(&String::from_utf8_lossy(&output.stdout))?;
    let errors = layers::violations(&packages);
    if !errors.is_empty() {
        return Err(format!(
            "layer violations (docs/architecture.md):
  {}",
            errors.join(
                "
  "
            )
        ));
    }
    println!("layers ok: {} packages", packages.len());
    Ok(layers::in_layer(&packages, layers::WASM_LAYER)
        .into_iter()
        .map(str::to_owned)
        .collect())
}

fn run(name: &str, program: &str, args: &[&str]) -> Result<(), String> {
    println!("== {name}: {program} {}", args.join(" "));
    let status = Command::new(program)
        .args(args)
        .current_dir(root())
        .status()
        .map_err(|e| format!("{name}: cannot run {program}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{name} failed ({status})"))
    }
}

/// The `version = "…"` line inside `[workspace.package]` of the root `Cargo.toml`: its index and value.
fn version_line(manifest: &str) -> Result<(usize, String), String> {
    let mut in_package = false;
    for (i, line) in manifest.lines().enumerate() {
        let t = line.trim();
        if t.starts_with('[') {
            in_package = t == "[workspace.package]";
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(value) = t
            .strip_prefix("version")
            .map(str::trim_start)
            .and_then(|r| r.strip_prefix('='))
        {
            let value = value.trim().trim_matches('"');
            return Ok((i, value.to_owned()));
        }
    }
    Err("no `version` in [workspace.package] of Cargo.toml".into())
}

fn manifest_path() -> PathBuf {
    root().join("Cargo.toml")
}

fn read_version() -> Result<String, String> {
    let manifest = std::fs::read_to_string(manifest_path()).map_err(|e| e.to_string())?;
    version_line(&manifest).map(|(_, v)| v)
}

/// `X.Y.Z` or `X.Y.Z-pre` with numeric parts and a pre-release of letters, digits, `.` and `-`.
fn valid_version(v: &str) -> bool {
    let (core, pre) = match v.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (v, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    let numeric = |p: &&str| {
        !p.is_empty()
            && p.bytes().all(|b| b.is_ascii_digit())
            && (p.len() == 1 || !p.starts_with('0'))
    };
    parts.len() == 3
        && parts.iter().all(numeric)
        && pre.is_none_or(|p| {
            !p.is_empty()
                && p.split('.').all(|id| {
                    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                })
        })
}

fn set_version(v: &str) -> Result<(), String> {
    if !valid_version(v) {
        return Err(format!("`{v}` is not X.Y.Z or X.Y.Z-pre"));
    }
    let path = manifest_path();
    let manifest = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let (index, old) = version_line(&manifest)?;
    let updated: Vec<String> = manifest
        .lines()
        .enumerate()
        .map(|(i, line)| {
            if i == index {
                format!("version = \"{v}\"")
            } else {
                line.to_owned()
            }
        })
        .collect();
    let mut text = updated.join("\n");
    if manifest.ends_with('\n') {
        text.push('\n');
    }
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    // Cargo.lock records each crate's version: refresh only the workspace's own entries.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    run("lockfile", &cargo, &["update", "--workspace"])?;
    println!("version {old} -> {v}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_version_of_the_workspace_package_only() {
        let manifest = "[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"1.2.3\"\nedition = \"2024\"\n\n[dependencies]\nversion = \"9\"\n";
        assert_eq!(version_line(manifest), Ok((4, "1.2.3".to_owned())));
        assert!(version_line("[package]\nversion = \"1.0.0\"\n").is_err());
    }

    #[test]
    fn accepts_release_and_pre_release_versions() {
        for v in ["0.0.0", "1.2.3", "10.20.30", "1.0.0-beta.1", "1.0.0-rc-2"] {
            assert!(valid_version(v), "{v}");
        }
        for v in [
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.2.x",
            "1.2.3-",
            "1.2.3-a..b",
            "1.2.3-ü",
            "",
        ] {
            assert!(!valid_version(v), "{v}");
        }
    }
}
