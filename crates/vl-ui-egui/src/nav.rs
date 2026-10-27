//! Top-level sections of the left rail.

use crate::strings;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Section {
    #[default]
    Home,
    Instances,
    Catalog,
    Account,
    Settings,
}

impl Section {
    pub const ALL: [Section; 5] = [
        Section::Home,
        Section::Instances,
        Section::Catalog,
        Section::Account,
        Section::Settings,
    ];

    /// The section with the given lowercase name (`home`, `instances`, ...).
    pub fn from_name(name: &str) -> Option<Section> {
        match name {
            "home" => Some(Section::Home),
            "instances" => Some(Section::Instances),
            "catalog" => Some(Section::Catalog),
            "account" => Some(Section::Account),
            "settings" => Some(Section::Settings),
            _ => None,
        }
    }

    /// The message id of the section's name.
    pub fn label_id(self) -> &'static str {
        match self {
            Section::Home => strings::NAV_HOME,
            Section::Instances => strings::NAV_INSTANCES,
            Section::Catalog => strings::NAV_CATALOG,
            Section::Account => strings::NAV_ACCOUNT,
            Section::Settings => strings::NAV_SETTINGS,
        }
    }
}
