//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/plist/bundle/BundleInfo.kt

mod categorization;
mod identification;
mod naming;
mod operating_system_version;
mod undocumented;

pub use categorization::Categorization;
pub use identification::Identification;
pub use naming::Naming;
pub use operating_system_version::OperatingSystemVersion;
pub use undocumented::Undocumented;

use super::{PlistDict, PropertyList};

/// An `Info.plist` model exposing exactly the five facets `BundleInfo` exposes in Kotlin,
/// all over the one shared dictionary. Constructing it converts nothing; each facet field
/// is a required delegate under a nullable declaration, so reading an absent key errors
/// (missing-key bug candidates A27/A28 in `docs/known-kotlin-bugs.md`). The five remaining
/// facet classes (`Help`, `Launch`, `LaunchConditions`, `Localization`, `Version`) are
/// unreachable from `BundleInfo` and constructed nowhere at `e47352f2`: not ported (P3).
#[derive(Clone, Debug, PartialEq)]
pub struct BundleInfo {
    dict: PlistDict,
}

impl BundleInfo {
    pub fn categorization(&self) -> Categorization {
        Categorization::wrap(self.dict.clone())
    }

    pub fn identification(&self) -> Identification {
        Identification::wrap(self.dict.clone())
    }

    pub fn naming(&self) -> Naming {
        Naming::wrap(self.dict.clone())
    }

    pub fn operating_system_version(&self) -> OperatingSystemVersion {
        OperatingSystemVersion::wrap(self.dict.clone())
    }

    pub fn undocumented(&self) -> Undocumented {
        Undocumented::wrap(self.dict.clone())
    }
}

impl PropertyList for BundleInfo {
    fn wrap(dict: PlistDict) -> Self {
        Self { dict }
    }

    fn dict(&self) -> &PlistDict {
        &self.dict
    }
}
