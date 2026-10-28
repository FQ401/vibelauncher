//! Quilt: `https://meta.quiltmc.org/v3`.

use crate::meta::Endpoints;

/// The Quilt meta server.
pub const META_URL: &str = "https://meta.quiltmc.org/v3";

/// The Quilt endpoints.
pub fn endpoints() -> Endpoints {
    Endpoints::new(META_URL)
}
