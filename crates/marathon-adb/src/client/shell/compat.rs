//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/request/shell/AsyncCompatShellCommandRequest.kt`.
//! adam portions: Copyright (C) 2019-2021 Anton Malinskiy; Apache-2.0.
//!
//! A shell stream that uses v2 when the device lists `shell_v2` and v1 otherwise (PLAN §3.9). Over v1,
//! each read becomes a stdout chunk of the bytes as read; adam decodes and re-encodes them (D11).

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;

use super::v1::RawChanneledShellCommandRequest;
use super::v2::{ChanneledShellCommandRequest, ShellCommandResultChunk};
use crate::client::AndroidDebugBridgeClient;
use crate::client::error::AdbError;
use crate::client::feature::Feature;
use crate::client::stream::ChannelStream;
use crate::client::target::Target;

/// What a compat request supplies; 6.6a's `TestRunnerRequest` is one.
pub trait AsyncCompatShellCommandRequest: Send + 'static {
    type Item: Send + 'static;

    fn cmd(&self) -> String;

    fn supported_features(&self) -> &[Feature];

    fn target(&self) -> Target {
        Target::NonSpecified
    }

    fn socket_idle_timeout(&self) -> Option<i64> {
        None
    }

    /// `convertChunk`: an item for the chunk, or nothing.
    fn convert_chunk(&mut self, chunk: ShellCommandResultChunk) -> Option<Self::Item>;

    /// `close(channel)`, a `suspend fun` as in adam: final items, sent only when the shell stream ends
    /// normally.
    fn close(&mut self, send: &mut VecDeque<Self::Item>) -> impl Future<Output = ()> + Send;
}

enum Shell {
    V2(ChannelStream<ChanneledShellCommandRequest>),
    V1(ChannelStream<RawChanneledShellCommandRequest>),
}

type Closing<R> = Pin<Box<dyn Future<Output = (R, VecDeque<<R as AsyncCompatShellCommandRequest>::Item>)> + Send>>;

/// The converted items; it fails with the shell stream's error, without running `close`.
///
/// Dropping a `next()` future loses nothing: the shell streams keep their pulls, and a `close` in progress
/// lives in the stream, as adam runs it in its producer whatever the receiver does.
pub struct CompatShellStream<R: AsyncCompatShellCommandRequest> {
    /// `None` while `close` runs, which owns it.
    request: Option<R>,
    shell: Option<Shell>,
    pending: VecDeque<R::Item>,
    state: CompatState<R>,
}

enum CompatState<R: AsyncCompatShellCommandRequest> {
    Streaming,
    Closing(Closing<R>),
    Done,
}

impl<R: AsyncCompatShellCommandRequest> CompatShellStream<R> {
    pub(crate) fn start(client: &AndroidDebugBridgeClient, request: R, serial: Option<&str>) -> Self {
        let (cmd, target, idle) = (request.cmd(), request.target(), request.socket_idle_timeout());
        // Neither inner request validates. The stdin channel Kotlin passes over v2 is never written or
        // closed, which is the same as none.
        let shell = if request.supported_features().contains(&Feature::ShellV2) {
            Shell::V2(client.open_channel(ChanneledShellCommandRequest::with(cmd, None, target, idle), serial))
        } else {
            Shell::V1(client.open_channel(RawChanneledShellCommandRequest::new(cmd, target, idle), serial))
        };
        Self {
            request: Some(request),
            shell: Some(shell),
            pending: VecDeque::new(),
            state: CompatState::Streaming,
        }
    }

    pub async fn next(&mut self) -> Option<Result<R::Item, AdbError>> {
        loop {
            if let Some(item) = self.pending.pop_front() {
                return Some(Ok(item));
            }
            match &mut self.state {
                CompatState::Streaming => {}
                CompatState::Closing(closing) => {
                    let (request, items) = closing.await;
                    self.request = Some(request);
                    self.pending.extend(items);
                    self.state = CompatState::Done;
                    continue;
                }
                CompatState::Done => return None,
            }
            let chunk = match self.shell.as_mut()? {
                Shell::V2(stream) => stream.next().await,
                Shell::V1(stream) => stream.next().await.map(|r| {
                    r.map(|stdout| ShellCommandResultChunk {
                        stdout: Some(stdout),
                        ..Default::default()
                    })
                }),
            };
            // Only `close` takes the request, and the stream is no longer streaming then.
            let request = self.request.as_mut()?;
            match chunk {
                Some(Ok(chunk)) => self.pending.extend(request.convert_chunk(chunk)),
                Some(Err(e)) => {
                    self.state = CompatState::Done;
                    return Some(Err(e));
                }
                None => {
                    let mut request = self.request.take()?;
                    self.state = CompatState::Closing(Box::pin(async move {
                        let mut items = VecDeque::new();
                        request.close(&mut items).await;
                        (request, items)
                    }));
                }
            }
        }
    }

    /// Cancel the owned transport, including an in-flight pull; no finalization on cancellation.
    pub fn cancel(&mut self) {
        // Instrumentation has no input writer. Dropping its owned pull closes the socket.
        self.shell.take();
        self.pending.clear();
        self.state = CompatState::Done;
    }

    pub fn request_mut(&mut self) -> Option<&mut R> {
        self.request.as_mut()
    }

    /// The request, for what it accumulated; `None` only while its `close` runs.
    pub fn request(&self) -> Option<&R> {
        self.request.as_ref()
    }
}

/// A compat request that hands the chunks on unchanged.
#[derive(Clone, Debug)]
pub struct CompatShellCommandRequest {
    pub cmd: String,
    pub supported_features: Vec<Feature>,
    pub target: Target,
    pub socket_idle_timeout: Option<i64>,
}

impl CompatShellCommandRequest {
    pub fn new(cmd: impl Into<String>, supported_features: Vec<Feature>) -> Self {
        Self {
            cmd: cmd.into(),
            supported_features,
            target: Target::NonSpecified,
            socket_idle_timeout: None,
        }
    }
}

impl AsyncCompatShellCommandRequest for CompatShellCommandRequest {
    type Item = ShellCommandResultChunk;

    fn cmd(&self) -> String {
        self.cmd.clone()
    }

    fn supported_features(&self) -> &[Feature] {
        &self.supported_features
    }

    fn target(&self) -> Target {
        self.target.clone()
    }

    fn socket_idle_timeout(&self) -> Option<i64> {
        self.socket_idle_timeout
    }

    fn convert_chunk(&mut self, chunk: ShellCommandResultChunk) -> Option<ShellCommandResultChunk> {
        Some(chunk)
    }

    async fn close(&mut self, _send: &mut VecDeque<ShellCommandResultChunk>) {}
}

/// Additive adapter for parsers whose convertChunk can throw. Transport and
/// conversion errors remain errors; normal-close finalization is never called after either.
pub trait FallibleAsyncCompatShellCommandRequest: Send + 'static {
    type Item: Send + 'static;
    fn cmd(&self) -> String;
    fn supported_features(&self) -> &[Feature];
    fn target(&self) -> Target {
        Target::NonSpecified
    }
    fn socket_idle_timeout(&self) -> Option<i64> {
        None
    }
    fn convert_chunk(&mut self, chunk: ShellCommandResultChunk) -> Result<Option<Self::Item>, AdbError>;
    fn close(&mut self, send: &mut VecDeque<Self::Item>) -> impl Future<Output = ()> + Send;
}
struct FallibleAdapter<R>(R);
impl<R: FallibleAsyncCompatShellCommandRequest> AsyncCompatShellCommandRequest for FallibleAdapter<R> {
    type Item = Result<R::Item, AdbError>;
    fn cmd(&self) -> String {
        self.0.cmd()
    }
    fn supported_features(&self) -> &[Feature] {
        self.0.supported_features()
    }
    fn target(&self) -> Target {
        self.0.target()
    }
    fn socket_idle_timeout(&self) -> Option<i64> {
        self.0.socket_idle_timeout()
    }
    fn convert_chunk(&mut self, chunk: ShellCommandResultChunk) -> Option<Self::Item> {
        self.0.convert_chunk(chunk).transpose()
    }
    async fn close(&mut self, send: &mut VecDeque<Self::Item>) {
        let mut items = VecDeque::new();
        self.0.close(&mut items).await;
        send.extend(items.into_iter().map(Ok));
    }
}
pub struct FallibleCompatShellStream<R: FallibleAsyncCompatShellCommandRequest> {
    inner: CompatShellStream<FallibleAdapter<R>>,
}
impl<R: FallibleAsyncCompatShellCommandRequest> FallibleCompatShellStream<R> {
    pub fn start(client: &AndroidDebugBridgeClient, request: R, serial: Option<&str>) -> Self {
        Self {
            inner: CompatShellStream::start(client, FallibleAdapter(request), serial),
        }
    }
    pub async fn next(&mut self) -> Option<Result<R::Item, AdbError>> {
        let item = self
            .inner
            .next()
            .await
            .map(|item| item.and_then(std::convert::identity));
        if matches!(item, Some(Err(_))) {
            self.inner.cancel();
        }
        item
    }
    pub fn request(&self) -> Option<&R> {
        self.inner.request().map(|r| &r.0)
    }
    pub fn request_mut(&mut self) -> Option<&mut R> {
        self.inner.request_mut().map(|r| &mut r.0)
    }
    pub fn cancel(&mut self) {
        self.inner.cancel();
    }
}
