//! The abstraction over "which mod owns this class".
//!
//! The class index of installed jars is built elsewhere; this crate only asks questions.

/// Answers which mods contain a class.
pub trait ClassIndex {
    /// Ids of the mods that contain `class` (a dotted binary name such as `com.example.Foo`).
    /// More than one id means the class cannot be attributed to a single mod.
    fn owners(&self, class: &str) -> Vec<String>;
}

/// A mod of the instance with the Java packages its jar contains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModInfo {
    pub id: String,
    pub name: String,
    /// Package prefixes in dotted form (`com.example.mod`); a slash form is accepted too.
    pub packages: Vec<String>,
}

impl ClassIndex for [ModInfo] {
    /// The mods with the longest matching package prefix.
    fn owners(&self, class: &str) -> Vec<String> {
        let mut best = 0usize;
        let mut owners: Vec<String> = Vec::new();
        for info in self {
            for package in &info.packages {
                let package = package.replace('/', ".");
                let package = package.trim_matches('.');
                if package.is_empty() || !is_in_package(class, package) {
                    continue;
                }
                if package.len() > best {
                    best = package.len();
                    owners.clear();
                }
                if package.len() == best && !owners.contains(&info.id) {
                    owners.push(info.id.clone());
                }
            }
        }
        owners
    }
}

impl ClassIndex for Vec<ModInfo> {
    fn owners(&self, class: &str) -> Vec<String> {
        self.as_slice().owners(class)
    }
}

fn is_in_package(class: &str, package: &str) -> bool {
    class
        .strip_prefix(package)
        .is_some_and(|rest| rest.starts_with('.'))
}
