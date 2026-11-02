//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/device/DeviceActor.kt,
//! DeviceAction.kt, DeviceEvent.kt and DeviceState.kt.
//!
//! These are the actor's own events; the provider's connect/disconnect events
//! are `crate::vendor::DeviceEvent`.
mod actor;
mod state;
pub use actor::{DeviceActor, DeviceActorRef};
pub use state::{DeviceAction, DeviceEvent, DeviceState, Running};
