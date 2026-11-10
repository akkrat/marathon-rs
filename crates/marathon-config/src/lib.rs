//! Port of configuration/src/main/kotlin/com/malinskiy/marathon/config/Configuration.kt
//!
//! WP 0.6 freezes ownership and type boundaries, not binding/default behaviour.
//! WP 2.x fills the private section representations and implements the profiles.

pub mod binding;
pub mod schema;
pub use schema::*;

pub mod bind;
pub mod bind_devices;
pub mod bind_vendor;
pub mod bundle;
pub mod devices;
pub mod dump;
pub use devices::*;
pub mod binder;
pub mod dynamic;
mod paths;
pub mod vendor;
pub use vendor::*;
pub mod loader;
pub mod pipeline;
pub mod render;
pub use render::serialize;
mod scalar;
pub mod yaml;
pub mod yaml_views;
pub use snakeyaml::{node, tokens};

pub mod tree;
