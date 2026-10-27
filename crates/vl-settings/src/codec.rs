//! TOML encoding of settings with per-group recovery.

use serde::Serialize;
use serde::de::DeserializeOwned;
use vl_core::settings::{
    CommandSettings, ConsoleSettings, EnvSettings, JavaSettings, MemorySettings, NativesSettings,
    Overrides, PerformanceSettings, SCHEMA_VERSION, Settings, WindowSettings,
};

const VERSION_KEY: &str = "schema_version";

/// A decoded file: the value and what had to be repaired.
pub(crate) struct Decoded<T> {
    pub value: T,
    pub warnings: Vec<String>,
}

/// Removes the groups of `table` that do not parse as their type, with a warning each.
fn drop_malformed(table: &mut toml::Table, warnings: &mut Vec<String>) {
    macro_rules! check {
        ($($name:literal => $ty:ty),*) => {$(
            if let Some(v) = table.get($name) {
                if v.clone().try_into::<$ty>().is_err() {
                    warnings.push(format!("group {} is malformed; using its default", $name));
                    table.remove($name);
                }
            }
        )*};
    }
    check!(
        "java" => JavaSettings,
        "memory" => MemorySettings,
        "window" => WindowSettings,
        "console" => ConsoleSettings,
        "commands" => CommandSettings,
        "env" => EnvSettings,
        "performance" => PerformanceSettings,
        "natives" => NativesSettings
    );
}

fn decode<T: DeserializeOwned + Default>(text: &str) -> Decoded<T> {
    let mut warnings = Vec::new();
    let mut table: toml::Table = match text.parse() {
        Ok(t) => t,
        Err(e) => {
            warnings.push(format!("file is not valid TOML ({e}); using defaults"));
            return Decoded {
                value: T::default(),
                warnings,
            };
        }
    };
    match table.remove(VERSION_KEY) {
        None => {}
        Some(toml::Value::Integer(v)) if v == i64::from(SCHEMA_VERSION) => {}
        Some(toml::Value::Integer(v)) if v > i64::from(SCHEMA_VERSION) => warnings.push(format!(
            "file uses schema version {v}, newer than {SCHEMA_VERSION}; unknown fields are kept"
        )),
        Some(_) => warnings.push(format!(
            "{VERSION_KEY} is invalid; assuming {SCHEMA_VERSION}"
        )),
    }
    drop_malformed(&mut table, &mut warnings);
    let value = table.try_into::<T>().unwrap_or_else(|e| {
        warnings.push(format!(
            "file does not match the settings layout ({e}); using defaults"
        ));
        T::default()
    });
    Decoded { value, warnings }
}

/// Decodes the global settings; out-of-range groups become defaults.
pub(crate) fn decode_settings(text: &str) -> Decoded<Settings> {
    let mut d = decode::<Settings>(text);
    let more = d.value.sanitize();
    d.warnings.extend(more);
    d
}

/// Decodes an instance's overrides; out-of-range groups are dropped.
pub(crate) fn decode_overrides(text: &str) -> Decoded<Overrides> {
    let mut d = decode::<Overrides>(text);
    let more = d.value.sanitize();
    d.warnings.extend(more);
    d
}

/// Encodes `value` as TOML text starting with the schema version.
pub(crate) fn encode<T: Serialize>(value: &T, schema_version: i64) -> Result<String, String> {
    let mut table = toml::Table::new();
    table.insert(VERSION_KEY.into(), toml::Value::Integer(schema_version));
    let body = toml::Table::try_from(value).map_err(|e| e.to_string())?;
    table.extend(body);
    toml::to_string_pretty(&table).map_err(|e| e.to_string())
}

/// The schema version declared by `text`, if it declares a valid one.
pub(crate) fn declared_version(text: &str) -> Option<i64> {
    let table: toml::Table = text.parse().ok()?;
    table.get(VERSION_KEY)?.as_integer()
}
