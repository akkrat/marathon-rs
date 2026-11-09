//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/AppleSimulatorProvider.kt:224-264
//! (`readSocket` and `ByteBufferWrapper`).
//!
//! A pure incremental decoder for the dynamic device-provider frame
//! format (PLAN §3.3): 4 reserved bytes, a big-endian signed `i32`
//! length (probe: `bundle-probe` `ktor-facts`), then that many payload
//! bytes, decoded as UTF-8 with replacement, substituted and bound as P2
//! by `pipeline::bind_devices_bytes`. No sockets, no runtime dependency:
//! WP 5.5a owns the TCP client, the refused-connection retry and what
//! each failure does to the producer (known bug A1: it stops for good).
//!
//! S5: the length is validated up front (`0..=2^20`, the Kotlin accept
//! boundary, with its exact too-long message) into typed failures,
//! instead of reproducing `ByteBuffer.limit(negative)` and the
//! power-of-two reallocation steps.
//!
//! Every close is a failure, a close on a frame boundary included:
//! `while (!socket.isClosed)` stays true after the peer closes, so the
//! next `readFully` throws (probe `frame-eof-probe`, a real ktor socket).

use crate::{
    MarathonDevices,
    binding::{BindingContext, ConfigurationError},
    pipeline::{LoadError, bind_devices_bytes},
};
use jcompat::SubstitutionError;
use marathon_error::FailureKind;

/// The largest accepted payload: Kotlin reallocates only up to `2^20`.
pub const MAX_FRAME_PAYLOAD: i32 = 1 << 20;

/// Which frame segment a close truncated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameSegment {
    Reserved,
    Length,
    Payload,
}

/// One frame's failure. The kinds stay separate because they map to
/// distinct Kotlin exceptions at the provider's `catch`; WP 5.5a decides
/// the production behaviour for each. `java_class` names the exception
/// Kotlin surfaces for the non-binding kinds.
#[derive(Debug)]
pub enum FrameError {
    /// Length above `2^20`: Kotlin `RuntimeException` with its message.
    TooLong { length: i32 },
    /// Negative length: Kotlin `IllegalArgumentException` from
    /// `ByteBuffer.limit` (S5 keeps the class, not the buffer steps).
    NegativeLength { length: i32 },
    /// The stream closed: ktor's `ClosedReceiveChannelException` from
    /// `readFully`/`readInt`. A close on a frame boundary is `Reserved`
    /// with nothing received.
    Truncated {
        segment: FrameSegment,
        expected: usize,
        received: usize,
    },
    /// Commons-text substitution cycle, unwrapped in Kotlin.
    Substitution(SubstitutionError),
    /// P2 binding failure with its original Jackson class/cause, so the
    /// provider can apply its `JsonMappingException`-only wrapping.
    Binding(Box<ConfigurationError>),
    /// The payload bound to a null document; Kotlin's `.workers` access
    /// on the null `readValue` result is the caller's crash to model.
    NullDocument,
    /// The binding worker could not be created; no binding took place.
    Worker(std::io::Error),
}

impl FrameError {
    /// The JVM exception class Kotlin surfaces, for log/report parity.
    /// `None` for binding failures, whose class is the carried
    /// `ConfigurationError`'s own (a `String`, not a `&'static str`).
    pub fn failure_kind(&self) -> Option<FailureKind> {
        match self {
            Self::TooLong { .. } => Some(FailureKind::Runtime),
            Self::NegativeLength { .. } => Some(FailureKind::IllegalArgument),
            Self::Truncated { .. } => Some(FailureKind::ChannelReceiveClosed),
            Self::Substitution(_) => Some(FailureKind::IllegalState),
            Self::Binding(_) => None,
            Self::NullDocument => Some(FailureKind::NullReference),
            Self::Worker(_) => Some(FailureKind::Io),
        }
    }

    /// The verbatim JVM message for the read-side kinds (probe
    /// `frame-eof-probe`); the other kinds' messages are this port's.
    pub fn message(&self) -> String {
        match self {
            Self::TooLong { .. } => "Device provider update too long: maximum is 2^20 bytes".to_owned(),
            Self::NegativeLength { length } => format!("newLimit < 0: ({length} < 0)"),
            // ktor's readInt names the whole int, readFully what is missing.
            Self::Truncated {
                segment: FrameSegment::Length,
                ..
            } => "EOF while 4 bytes expected".to_owned(),
            Self::Truncated { expected, received, .. } => {
                format!("Unexpected EOF: expected {} more bytes", expected - received)
            }
            Self::Substitution(error) => error.message.to_owned(),
            Self::Binding(error) => error.message.as_ref().map_or_else(String::new, |m| m.to_owned()),
            Self::NullDocument => "device provider update is a null document".to_owned(),
            Self::Worker(error) => error.to_string(),
        }
    }
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for FrameError {}

/// Chunk-fed frame decoder. Feed arbitrary splits/coalescing; each feed
/// returns every frame completed by it, in order. A failure is terminal
/// for the Kotlin producer: after one, the decoder is dead and further
/// feeds yield nothing.
#[derive(Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
    failed: bool,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a terminal failure has been yielded.
    pub fn is_failed(&self) -> bool {
        self.failed
    }

    /// Binds every completed frame through `pipeline::bind_devices_bytes`,
    /// which blocks on a 32 MiB-stack binding thread per frame: call it off
    /// the core executor (docs/design/lifecycle.md).
    pub fn feed(
        &mut self,
        chunk: &[u8],
        context: &BindingContext,
    ) -> Vec<std::result::Result<MarathonDevices, FrameError>> {
        let mut out = Vec::new();
        if self.failed {
            return out;
        }
        self.buffer.extend_from_slice(chunk);
        loop {
            if self.buffer.len() < 8 {
                break;
            }
            let length = i32::from_be_bytes([self.buffer[4], self.buffer[5], self.buffer[6], self.buffer[7]]);
            if length < 0 {
                self.failed = true;
                out.push(Err(FrameError::NegativeLength { length }));
                break;
            }
            if length > MAX_FRAME_PAYLOAD {
                self.failed = true;
                out.push(Err(FrameError::TooLong { length }));
                break;
            }
            let frame_len = 8 + length as usize;
            if self.buffer.len() < frame_len {
                break;
            }
            let payload: Vec<u8> = self.buffer.drain(..frame_len).skip(8).collect();
            match bind_devices_bytes(&payload, context) {
                Ok(devices) => out.push(Ok(devices)),
                Err(error) => {
                    self.failed = true;
                    out.push(Err(match error {
                        LoadError::Substitution(e) => FrameError::Substitution(e),
                        LoadError::Configuration(e) => FrameError::Binding(e),
                        LoadError::NullDocument => FrameError::NullDocument,
                        LoadError::Worker(e) => FrameError::Worker(e),
                    }));
                    break;
                }
            }
        }
        out
    }

    /// The close-of-stream outcome: `Some(Err(Truncated))` with the segment
    /// the close stopped in, a frame boundary (empty buffer) included, as
    /// Kotlin's reader throws there too and the producer stops (A1). `None`
    /// only after a failure was already yielded.
    pub fn finish(&mut self) -> Option<std::result::Result<MarathonDevices, FrameError>> {
        if self.failed {
            return None;
        }
        self.failed = true;
        let (segment, expected, received) = if self.buffer.len() < 4 {
            (FrameSegment::Reserved, 4, self.buffer.len())
        } else if self.buffer.len() < 8 {
            (FrameSegment::Length, 4, self.buffer.len() - 4)
        } else {
            let length = i32::from_be_bytes([self.buffer[4], self.buffer[5], self.buffer[6], self.buffer[7]]);
            (
                FrameSegment::Payload,
                usize::try_from(length).unwrap_or(0),
                self.buffer.len() - 8,
            )
        };
        Some(Err(FrameError::Truncated {
            segment,
            expected,
            received,
        }))
    }
}
