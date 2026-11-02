//! Port of core/src/main/kotlin/com/malinskiy/marathon/di/Modules.kt
//! Compile-only consumer for WP 2.5, 3.10 and all vendor construction WPs.

use marathon_cli::VendorFactories;
use marathon_core::{runtime::VendorFuture, vendor::VendorServices, wiring::VendorDependencies};

pub fn construct_every_vendor(
    factories: &VendorFactories,
    inputs: VendorDependencies,
) -> VendorFuture<'static, VendorServices> {
    factories.construct(inputs)
}
