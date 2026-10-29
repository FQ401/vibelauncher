//! `cargo xtask sources`: every package in `Cargo.lock` comes from the workspace or from
//! crates.io; git dependencies and other registries are refused.

use crate::root;

/// The crates.io index, as `Cargo.lock` records it (git and sparse protocol).
const CRATES_IO: [&str; 2] = [
    "registry+https://github.com/rust-lang/crates.io-index",
    "sparse+https://index.crates.io/",
];

/// Checks `Cargo.lock` of the workspace.
pub fn check() -> Result<(), String> {
    let path = root().join("Cargo.lock");
    let lock = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let foreign = foreign_sources(&lock);
    if !foreign.is_empty() {
        return Err(format!(
            "packages from outside crates.io in Cargo.lock:\n  {}",
            foreign.join("\n  ")
        ));
    }
    println!("sources ok: crates.io and the workspace only");
    Ok(())
}

/// `name version (source)` of each package whose source is not crates.io.
fn foreign_sources(lock: &str) -> Vec<String> {
    let mut found = Vec::new();
    let (mut name, mut version) = ("", "");
    for line in lock.lines() {
        let value = |key: &str| {
            line.strip_prefix(key)
                .map(str::trim_start)
                .and_then(|r| r.strip_prefix('='))
                .map(|v| v.trim().trim_matches('"'))
        };
        if line.trim() == "[[package]]" {
            (name, version) = ("", "");
        } else if let Some(v) = value("name") {
            name = v;
        } else if let Some(v) = value("version") {
            version = v;
        } else if let Some(source) = value("source")
            && !CRATES_IO.contains(&source)
        {
            found.push(format!("{name} {version} ({source})"));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_crates_io_and_path_packages_pass() {
        let lock = r#"version = 4

[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "00"

[[package]]
name = "vl-core"
version = "0.0.0"

[[package]]
name = "forked"
version = "0.1.0"
source = "git+https://github.com/example/forked?rev=abc#abc"

[[package]]
name = "other"
version = "2.0.0"
source = "registry+https://registry.example.test/index"
"#;
        assert_eq!(
            foreign_sources(lock),
            [
                "forked 0.1.0 (git+https://github.com/example/forked?rev=abc#abc)",
                "other 2.0.0 (registry+https://registry.example.test/index)"
            ]
        );
    }
}
