//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/remote/rsync/RsyncTarget.kt
use jcompat::writers::JavaString;
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RsyncTarget {
    pub addr: JavaString,
    pub port: i32,
}
