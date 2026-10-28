//! Fabric: `https://meta.fabricmc.net/v2`.

use crate::meta::Endpoints;

/// The Fabric meta server.
pub const META_URL: &str = "https://meta.fabricmc.net/v2";

/// The Fabric endpoints.
pub fn endpoints() -> Endpoints {
    Endpoints::new(META_URL)
}
