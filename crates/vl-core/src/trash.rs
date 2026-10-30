//! The launcher's own trash of deleted instances.
//!
//! A deleted instance is kept in the trash with what is known about it ([`TrashItem`]) until it
//! is restored, purged by hand, or purged by the limits of the `storage` settings group: items
//! older than [`StorageSettings::trash_days`](crate::settings::StorageSettings) days first, then
//! the oldest items while the trash is larger than
//! [`StorageSettings::trash_max_gib`](crate::settings::StorageSettings). The trash commands are
//! instance commands ([`InstanceCommand`](crate::instance::InstanceCommand)); a change is
//! announced with [`InstanceEvent::TrashChanged`](crate::instance::InstanceEvent).

use serde::{Deserialize, Serialize};

use crate::instance::Loader;

/// Days a deleted instance is kept by default.
pub const DEFAULT_KEEP_DAYS: u32 = 30;
/// Size limit of the trash by default, GiB.
pub const DEFAULT_MAX_GIB: u32 = 20;
/// Longest comment kept with a deleted instance, in characters.
pub const COMMENT_MAX_CHARS: usize = 1000;

/// One deleted instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrashItem {
    /// The item's identifier in the trash (a plain folder name).
    pub id: String,
    /// The id the instance had; a restore uses it again when it is free.
    pub instance: String,
    /// The display name the instance had.
    pub name: String,
    /// The group the instance was shown in.
    pub group: Option<String>,
    /// The game version id, when known.
    pub game_version: String,
    /// The mod loader, when known.
    pub loader: Loader,
    /// When the instance was deleted, Unix seconds.
    pub deleted_unix: i64,
    /// Bytes the instance takes.
    pub size: u64,
    /// What the user wrote when deleting it.
    pub comment: String,
    /// Why part of the record is missing (damaged or missing metadata), if it is.
    pub problem: Option<String>,
}

/// The trash as it is now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrashList {
    /// The deleted instances, the most recently deleted first.
    pub items: Vec<TrashItem>,
    /// Bytes all items take.
    pub size: u64,
    /// Days an item is kept (the `storage.trash_days` setting).
    pub keep_days: u32,
    /// Size limit of the trash, GiB (the `storage.trash_max_gib` setting).
    pub max_gib: u32,
}

/// What a purge removes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PurgeTarget {
    /// One item, by its id.
    Item(String),
    /// Every item.
    All,
    /// The items beyond the limits of the `storage` settings: too old, then the oldest while the
    /// trash is too large. The most recently deleted item is never removed for size.
    Limits,
}

/// What a purge did.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PurgeReport {
    /// The ids of the items removed.
    pub removed: Vec<String>,
    /// Bytes freed.
    pub freed: u64,
    /// Items that could not be removed, with the reason.
    pub failed: Vec<(String, String)>,
}
