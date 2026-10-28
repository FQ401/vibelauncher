//! The class index: which mod owns which Java package.

use vl_crash::{ClassIndex, ModInfo};

use crate::jar::JarInfo;

/// Maps classes to the mods of an instance by package prefix, for crash diagnosis.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModClassIndex {
    mods: Vec<ModInfo>,
}

impl ModClassIndex {
    /// An index with no mods.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a jar. Each mod it declares owns the jar's packages; a jar without metadata is
    /// listed under `fallback_id` (its file name).
    pub fn add_jar(&mut self, fallback_id: &str, jar: &JarInfo) {
        if jar.packages.is_empty() {
            return;
        }
        if jar.mods.is_empty() {
            self.push(fallback_id, fallback_id, &jar.packages);
        }
        for m in &jar.mods {
            self.push(&m.id, &m.name, &jar.packages);
        }
    }

    fn push(&mut self, id: &str, name: &str, packages: &[String]) {
        // The same mod can be declared for several loaders in one jar.
        if let Some(existing) = self.mods.iter_mut().find(|m| m.id == id) {
            for p in packages {
                if !existing.packages.contains(p) {
                    existing.packages.push(p.clone());
                }
            }
            return;
        }
        self.mods.push(ModInfo {
            id: id.to_owned(),
            name: name.to_owned(),
            packages: packages.to_vec(),
        });
    }

    /// The indexed mods.
    pub fn mods(&self) -> &[ModInfo] {
        &self.mods
    }
}

impl ClassIndex for ModClassIndex {
    fn owners(&self, class: &str) -> Vec<String> {
        self.mods.owners(class)
    }
}
