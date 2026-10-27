//! VibeLauncher core types.

/// Identifier of a launcher instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceId(String);

impl InstanceId {
    /// Creates an identifier; empty or whitespace-only input is rejected.
    pub fn new(value: &str) -> Option<Self> {
        let value = value.trim();
        (!value.is_empty()).then(|| Self(value.to_owned()))
    }

    /// The identifier as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_and_rejects_empty_ids() {
        assert_eq!(InstanceId::new("  main ").unwrap().as_str(), "main");
        assert!(InstanceId::new("   ").is_none());
    }
}
