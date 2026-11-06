//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/device/ConnectionFailures.kt

use super::connection_factory::ConnectionError;
use crate::{
    cmd::remote::ssh::sshj::{DisconnectReason, error::*},
    logparser::parser::DeviceFailureReason,
};

pub fn is_transient_connection_failure(error: &ConnectionError) -> bool {
    let ConnectionError::Device(error) = error else {
        return false;
    };
    if error.reason != DeviceFailureReason::UnreachableHost {
        return false;
    }
    let Some(mut cause) = error.cause.as_ref() else {
        return true;
    };
    for _ in 0..16 {
        // D35: a failed negotiation has reason UNKNOWN, like real network faults, but the Kotlin KDoc's "failed key
        // exchange" will not heal by itself (A16).
        if (cause.is_instance(TRANSPORT_EXCEPTION)
            && !cause.is_no_common_algorithm()
            && matches!(
                cause.reason,
                DisconnectReason::Unknown
                    | DisconnectReason::ConnectionLost
                    | DisconnectReason::TooManyConnections
                    | DisconnectReason::ByApplication
                    | DisconnectReason::ServiceNotAvailable
                    | DisconnectReason::MacError
            ))
            || [
                SOCKET_EXCEPTION,
                SOCKET_TIMEOUT_EXCEPTION,
                UNKNOWN_HOST_EXCEPTION,
                TIMEOUT_EXCEPTION,
            ]
            .iter()
            .any(|class| cause.is_instance(class))
        {
            return true;
        }
        let Some(next) = cause.cause.as_deref() else {
            break;
        };
        // Owned throwable chains cannot contain the JVM's immediate self-link.
        cause = next;
    }
    false
}
