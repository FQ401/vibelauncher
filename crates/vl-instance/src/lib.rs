//! Instances on disk.
//!
//! - The instance format: a config file (name, icon, group, play time), components (game version,
//!   loaders, patches) and the game directory.
//! - Create, list (lazily), rename (with the directory) and delete (to the OS trash when there is
//!   one). A damaged instance is listed with a problem and never breaks the list.
