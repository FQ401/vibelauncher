//! Directory names derived from display names.

const MAX_LEN: usize = 64;
const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// A directory name that is valid on every supported OS, made from a display name.
pub fn dir_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .take(MAX_LEN)
        .collect();
    let trimmed = cleaned.trim_matches(|c: char| c == '.' || c.is_whitespace());
    let mut out = if trimmed.is_empty() {
        "instance".to_owned()
    } else {
        trimmed.to_owned()
    };
    let stem = out.split('.').next().unwrap_or("").to_lowercase();
    if RESERVED.contains(&stem.as_str()) {
        out.insert(0, '_');
    }
    out
}

/// Whether `id` is a single plain directory name (no separators, no traversal).
pub fn is_plain_name(id: &str) -> bool {
    !id.is_empty() && !id.starts_with('.') && !id.contains(['/', '\\', ':', '\0'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes() {
        assert_eq!(dir_name("My: pack/1"), "My_ pack_1");
        assert_eq!(dir_name("  ..  "), "instance");
        assert_eq!(dir_name("CON"), "_CON");
        assert_eq!(dir_name("nul.txt"), "_nul.txt");
        assert_eq!(dir_name(&"a".repeat(200)).len(), 64);
    }

    #[test]
    fn plain_names() {
        assert!(is_plain_name("Survival"));
        assert!(!is_plain_name(".."));
        assert!(!is_plain_name("a/b"));
        assert!(!is_plain_name(".hidden"));
        assert!(!is_plain_name(""));
    }
}
