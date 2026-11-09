//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/request/Request.kt`,
//! `.../request/ComplexRequest.kt`, `.../request/AsyncChannelRequest.kt` and
//! `.../request/SynchronousRequest.kt`.
//! adam portions: Copyright (C) 2019-2021 Anton Malinskiy; Apache-2.0.
//!
//! The traits mirror adam's classes so that 6.2 can port its request types one for one (WP 6.1
//! orchestrator decision 2): a request serializes itself, may validate itself, and either reads one
//! result ([`ComplexRequest`]) or streams items ([`AsyncChannelRequest`]).

use std::collections::VecDeque;
use std::future::Future;

use jcompat::logging::Level;
use tokio::sync::mpsc;

use super::consts::MAX_PACKET_LENGTH;
use super::error::AdbError;
use super::log::REQUEST_LOGGER;
use super::target::Target;
use super::transform::ResponseTransformer;
use super::transport::{Socket, SocketWriter};
use super::validation::ValidationResponse;

/// The longest payload a four-hex-digit header can describe.
const MAX_REQUEST_PAYLOAD: usize = 0xFFFF;

/// `Request`: what every request has.
pub trait Request: Send {
    /// `serialize()`: the framed request, usually [`create_base_request`].
    fn serialize(&self) -> Vec<u8>;

    fn validate(&self) -> ValidationResponse {
        ValidationResponse::SUCCESS
    }

    /// `socketIdleTimeout` in milliseconds (D21); `None` uses the client's.
    fn socket_idle_timeout(&self) -> Option<i64> {
        None
    }

    /// `javaClass.simpleName`, which the validation error names. The default is the Rust type's name,
    /// which matches adam's for the ported requests.
    fn simple_name(&self) -> &str {
        simple_type_name(std::any::type_name::<Self>())
    }

    /// `handshake`: sends the request and expects OKAY.
    fn handshake(&mut self, socket: &mut Socket) -> impl Future<Output = Result<(), AdbError>> + Send {
        let request = self.serialize();
        async move { handshake(&request, socket).await }
    }
}

/// A request that reads one result after its handshake.
pub trait ComplexRequest: Request {
    type Output: Send;

    /// Some requests skip the OKAY and read the reply directly; they override this.
    fn process(&mut self, socket: &mut Socket) -> impl Future<Output = Result<Self::Output, AdbError>> + Send {
        async move {
            self.handshake(socket).await?;
            self.read_element(socket).await
        }
    }

    fn read_element(&mut self, socket: &mut Socket) -> impl Future<Output = Result<Self::Output, AdbError>> + Send;
}

/// A request whose reply is a stream of items, optionally with an input channel written back
/// concurrently (shell v2 stdin).
pub trait AsyncChannelRequest: Request + 'static {
    type Item: Send + 'static;
    type Input: Send + 'static;

    /// `channel`: the input, which the client takes once, after the handshake. Once it is closed and
    /// drained, the stream stops reading (`AndroidDebugBridgeClient.kt:95-97`, known bug N17).
    fn take_channel(&mut self) -> Option<mpsc::Receiver<Self::Input>> {
        None
    }

    /// Reads once, sending zero or more items to `send`. `Ok(true)` ends the stream.
    fn read_element(
        &mut self,
        socket: &mut Socket,
        send: &mut VecDeque<Self::Item>,
    ) -> impl Future<Output = Result<bool, AdbError>> + Send;

    /// Writes one input element. It runs concurrently with [`AsyncChannelRequest::read_element`], so it
    /// gets the socket's write side and not the request.
    fn write_element(element: Self::Input, socket: &SocketWriter) -> impl Future<Output = Result<(), AdbError>> + Send;

    /// `close(channel)`, a `suspend fun` as in adam: may send final items and may await, as 6.2's file
    /// requests do to close their files. Runs when the stream ends or fails after connecting, never when it
    /// is dropped; the connection is no longer available. An error is logged at DEBUG and ignored.
    fn close(&mut self, send: &mut VecDeque<Self::Item>) -> impl Future<Output = Result<(), AdbError>> + Send {
        let _ = send;
        async { Ok(()) }
    }
}

/// `createBaseRequest`: the target prefix and the request, behind a `%04X` length.
///
/// adam formats the UTF-16 length first, then overwrites the first four bytes with the UTF-8 byte length
/// (`Request.kt:59-67`); below 0x10000 bytes that is exactly `%04X` of the byte length. Longer requests
/// are refused when sent ([`handshake`], WP 6.1 P5).
pub fn create_base_request(target: &Target, request: &str) -> Vec<u8> {
    let full = target.serialize() + request;
    let mut out = format!("{:04X}", full.len()).into_bytes();
    out.extend_from_slice(full.as_bytes());
    out
}

/// The default handshake for an already serialized request.
pub async fn handshake(request: &[u8], socket: &mut Socket) -> Result<(), AdbError> {
    let payload = request.len().saturating_sub(4);
    if payload > MAX_REQUEST_PAYLOAD {
        // adam sends a corrupted five-digit header here; the server cannot take it either way (P5).
        return Err(AdbError::RequestRejected(format!(
            "Request of {payload} bytes is longer than the {MAX_REQUEST_PAYLOAD} bytes adb accepts"
        )));
    }
    socket.write(request).await?;
    let response = socket.read_transport_response().await;
    if !response.okay {
        if let Some(logging) = socket.logging() {
            logging.log(REQUEST_LOGGER, Level::Warn, None, || {
                format!(
                    "adb server rejected command {}",
                    String::from_utf8_lossy(request).into_owned()
                )
            });
        }
        return Err(AdbError::RequestRejected(
            response.message.unwrap_or_else(|| "no message received".into()),
        ));
    }
    Ok(())
}

/// `SynchronousRequest.readElement`: feeds every read, of at most 16 KiB, to `transformer` until the end
/// of the stream, then transforms.
pub async fn read_synchronous<T: ResponseTransformer + Send>(
    socket: &mut Socket,
    transformer: &mut T,
) -> Result<T::Output, AdbError> {
    let mut buffer = vec![0u8; MAX_PACKET_LENGTH];
    while !socket.is_closed_for_read() {
        let count = socket.read_available(&mut buffer).await;
        if count < 0 {
            break;
        }
        transformer.process(&buffer[..count as usize]);
    }
    transformer.transform()
}

/// The last path segment of a type name, without generic arguments.
fn simple_type_name(full: &'static str) -> &'static str {
    let base = full.split('<').next().unwrap_or(full);
    base.rsplit("::").next().unwrap_or(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_request_framing() {
        assert_eq!(create_base_request(&Target::Host, "version"), b"000Chost:version");
        // The length counts UTF-8 bytes, in uppercase hex.
        assert_eq!(
            create_base_request(&Target::NonSpecified, &"ж".repeat(10)),
            [b"0014".as_slice(), "ж".repeat(10).as_bytes()].concat()
        );
        assert_eq!(simple_type_name("a::b::ListDevicesRequest"), "ListDevicesRequest");
        assert_eq!(simple_type_name("a::Stream<a::b::C>"), "Stream");
    }
}
