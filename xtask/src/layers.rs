//! The workspace's layer rules (docs/architecture.md), checked from `cargo metadata`.
//!
//! Every workspace package except `xtask` is named `vl-<name>` and declares its layer in
//! `[package.metadata.vl] layer = "…"`. A package may depend on workspace packages of the layers
//! its own layer allows; `apps/` holds only `app` packages and `crates/` holds none.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

/// Each layer and the layers its packages may depend on.
const LAYERS: &[(&str, &[&str])] = &[
    ("core", &["core"]),
    ("native", &["core", "native"]),
    ("backend", &["core", "native"]),
    ("ui", &["core"]),
    ("app", &["core", "backend", "ui"]),
];

/// Extra layers a layer may use in dev-dependencies only (UI tests run on the mock backend).
const DEV_EXTRA: &[(&str, &[&str])] = &[("ui", &["backend"])];

/// The package that is outside the layers.
const TOOL: &str = "xtask";

/// The layer that must build for `wasm32-unknown-unknown`.
pub const WASM_LAYER: &str = "core";

/// A workspace package as the check sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// The package name.
    pub name: String,
    /// The declared layer.
    pub layer: Option<String>,
    /// The directory that holds the package's directory (`crates`, `apps`).
    pub group: String,
    /// Dependencies: package name and whether it is a dev-dependency.
    pub deps: Vec<(String, bool)>,
}

/// The workspace packages from `cargo metadata --no-deps --format-version 1` output.
pub fn packages(metadata: &str) -> Result<Vec<Package>, String> {
    let json: Value = serde_json::from_str(metadata).map_err(|e| format!("cargo metadata: {e}"))?;
    let list = json
        .get("packages")
        .and_then(Value::as_array)
        .ok_or("cargo metadata: no packages")?;
    list.iter()
        .map(|p| {
            let str_of =
                |v: &Value, key: &str| v.get(key).and_then(Value::as_str).map(str::to_owned);
            let name = str_of(p, "name").ok_or("cargo metadata: a package without a name")?;
            let layer = p
                .get("metadata")
                .and_then(|m| m.get("vl"))
                .and_then(|m| str_of(m, "layer"));
            let group = str_of(p, "manifest_path")
                .and_then(|m| {
                    let dir = Path::new(&m).parent()?.parent()?.file_name()?.to_str()?;
                    Some(dir.to_owned())
                })
                .unwrap_or_default();
            let deps = p
                .get("dependencies")
                .and_then(Value::as_array)
                .map(|deps| {
                    deps.iter()
                        .filter_map(|d| {
                            let dev = d.get("kind").and_then(Value::as_str) == Some("dev");
                            Some((str_of(d, "name")?, dev))
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok(Package {
                name,
                layer,
                group,
                deps,
            })
        })
        .collect()
}

fn allowed(table: &[(&str, &[&str])], layer: &str, dep_layer: &str) -> bool {
    table
        .iter()
        .any(|(l, deps)| *l == layer && deps.contains(&dep_layer))
}

/// Every violation of the layer rules, as readable lines.
pub fn violations(packages: &[Package]) -> Vec<String> {
    let layers: BTreeMap<&str, &str> = packages
        .iter()
        .filter_map(|p| Some((p.name.as_str(), p.layer.as_deref()?)))
        .collect();
    let mut errors = Vec::new();
    for p in packages.iter().filter(|p| p.name != TOOL) {
        if !p.name.starts_with("vl-") {
            errors.push(format!(
                "{}: workspace packages are named `vl-<name>`",
                p.name
            ));
        }
        let Some(layer) = p.layer.as_deref() else {
            errors.push(format!(
                "{}: no `[package.metadata.vl] layer` in its Cargo.toml",
                p.name
            ));
            continue;
        };
        if !LAYERS.iter().any(|(l, _)| *l == layer) {
            errors.push(format!("{}: unknown layer `{layer}`", p.name));
            continue;
        }
        match (p.group.as_str(), layer) {
            ("apps", "app") => {}
            ("apps", _) => errors.push(format!("{}: apps/ holds `app` packages only", p.name)),
            (_, "app") => errors.push(format!("{}: `app` packages live in apps/", p.name)),
            _ => {}
        }
        for (dep, dev) in &p.deps {
            let Some(dep_layer) = layers.get(dep.as_str()) else {
                continue; // not a workspace package
            };
            let ok =
                allowed(LAYERS, layer, dep_layer) || (*dev && allowed(DEV_EXTRA, layer, dep_layer));
            if !ok {
                let kind = if *dev { "dev-dependency" } else { "dependency" };
                errors.push(format!(
                    "{} ({layer}) -> {dep} ({dep_layer}): a {layer} package may not have a {kind} on a {dep_layer} package",
                    p.name
                ));
            }
        }
    }
    errors
}

/// The packages of `layer`.
pub fn in_layer<'a>(packages: &'a [Package], layer: &str) -> Vec<&'a str> {
    packages
        .iter()
        .filter(|p| p.layer.as_deref() == Some(layer))
        .map(|p| p.name.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(name: &str, layer: &str, group: &str, deps: &[(&str, bool)]) -> Package {
        Package {
            name: name.into(),
            layer: (!layer.is_empty()).then(|| layer.into()),
            group: group.into(),
            deps: deps.iter().map(|(d, dev)| ((*d).into(), *dev)).collect(),
        }
    }

    fn workspace(extra: Package) -> Vec<Package> {
        vec![
            pkg("vl-core", "core", "crates", &[("serde", false)]),
            pkg("vl-meta", "core", "crates", &[("vl-core", false)]),
            pkg("vl-net", "native", "crates", &[("vl-core", false)]),
            pkg("vl-backend", "backend", "crates", &[("vl-net", false)]),
            pkg("vl-mock", "backend", "crates", &[("vl-core", false)]),
            pkg(
                "vl-ui-egui",
                "ui",
                "crates",
                &[("vl-core", false), ("vl-mock", true)],
            ),
            pkg(
                "vl-cli",
                "app",
                "apps",
                &[("vl-backend", false), ("vl-mock", false)],
            ),
            pkg("xtask", "", "", &[("serde_json", false)]),
            extra,
        ]
    }

    #[test]
    fn the_intended_shape_passes() {
        let ws = workspace(pkg("vl-java", "native", "crates", &[("vl-net", false)]));
        assert_eq!(violations(&ws), Vec::<String>::new());
        assert_eq!(in_layer(&ws, WASM_LAYER), vec!["vl-core", "vl-meta"]);
    }

    #[test]
    fn upward_and_sideways_dependencies_fail() {
        for (bad, needle) in [
            (
                pkg("vl-x", "core", "crates", &[("vl-net", false)]),
                "(core) -> vl-net",
            ),
            (
                pkg("vl-x", "native", "crates", &[("vl-mock", false)]),
                "(native) -> vl-mock",
            ),
            (
                pkg("vl-x", "ui", "crates", &[("vl-net", false)]),
                "(ui) -> vl-net",
            ),
            (
                pkg("vl-x", "ui", "crates", &[("vl-backend", false)]),
                "(ui) -> vl-backend",
            ),
            (
                pkg("vl-x", "backend", "crates", &[("vl-mock", false)]),
                "(backend) -> vl-mock",
            ),
            (
                pkg("vl-x", "app", "apps", &[("vl-net", false)]),
                "(app) -> vl-net",
            ),
            (
                pkg("vl-x", "core", "crates", &[("vl-mock", true)]),
                "(core) -> vl-mock",
            ),
        ] {
            let errors = violations(&workspace(bad));
            assert!(
                errors.iter().any(|e| e.contains(needle)),
                "{needle}: {errors:?}"
            );
        }
    }

    #[test]
    fn names_layers_and_directories_are_checked() {
        let errors = violations(&workspace(pkg("launcher-x", "native", "crates", &[])));
        assert!(errors.iter().any(|e| e.contains("named `vl-<name>`")));
        let errors = violations(&workspace(pkg("vl-x", "", "crates", &[])));
        assert!(
            errors
                .iter()
                .any(|e| e.contains("no `[package.metadata.vl] layer`"))
        );
        let errors = violations(&workspace(pkg("vl-x", "gui", "crates", &[])));
        assert!(errors.iter().any(|e| e.contains("unknown layer")));
        let errors = violations(&workspace(pkg("vl-x", "app", "crates", &[])));
        assert!(errors.iter().any(|e| e.contains("live in apps/")));
        let errors = violations(&workspace(pkg("vl-x", "native", "apps", &[])));
        assert!(errors.iter().any(|e| e.contains("apps/ holds")));
    }

    #[test]
    fn reads_cargo_metadata() {
        let json = r#"{"packages":[{"name":"vl-net","manifest_path":"/w/crates/vl-net/Cargo.toml",
            "metadata":{"vl":{"layer":"native"}},
            "dependencies":[{"name":"vl-core","kind":null},{"name":"vl-mock","kind":"dev"}]}]}"#;
        let packages = packages(json).unwrap();
        assert_eq!(
            packages,
            vec![pkg(
                "vl-net",
                "native",
                "crates",
                &[("vl-core", false), ("vl-mock", true)]
            )]
        );
        assert!(super::packages("{}").is_err());
    }
}
