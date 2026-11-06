//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/AndroidDebugBridgeClient.kt:64-116`, the
//! `produce` loop behind `execute(AsyncChannelRequest)`, as a pull-based stream.
//! adam portions: Copyright (C) 2019-2021 Anton Malinskiy; Apache-2.0.
//!
//! Kotlin's producer runs on its own and keeps one item read ahead; here each pull does the reading, so
//! nothing runs while nobody asks, except the input writer of a request that has an input channel.
//! Dropping the stream closes the connection and aborts that writer. A pull in progress lives inside the
//! stream, so dropping a `next()` future (in a `select!`, say) loses nothing.
//!
//! The input writer starts after the handshake, on the first pull: until the stream is pulled, nothing
//! drains the request's input channel, so a sender that waits for room before that needs a channel large
//! enough or a task of its own, or it deadlocks against the caller's first `next()`.

use std::collections::VecDeque;
use std::future::{Future, poll_fn};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use jcompat::logging::Level;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use super::error::AdbError;
use super::log::{AdamLogging, CLIENT_LOGGER};
use super::misc::SetDeviceRequest;
use super::request::{AsyncChannelRequest, Request};
use super::transport::{Socket, SocketFactory};

type Pull<R> = Pin<Box<dyn Future<Output = Step<R>> + Send>>;

/// The items of an [`AsyncChannelRequest`], ended by `None` or by one error. Nothing happens until the
/// first pull, not even writing the request's input (see the module docs).
pub struct ChannelStream<R: AsyncChannelRequest> {
    pending: VecDeque<R::Item>,
    state: State<R>,
}

enum State<R: AsyncChannelRequest> {
    Pulling(Pull<R>),
    Failed(AdbError),
    Done,
}

enum Step<R: AsyncChannelRequest> {
    Continue(Box<Session<R>>, VecDeque<R::Item>),
    Finished(VecDeque<R::Item>, Option<AdbError>),
}

struct Session<R: AsyncChannelRequest> {
    request: R,
    socket: Socket,
    writer: Option<InputWriter>,
    logging: Option<AdamLogging>,
}

/// The `backChannelJob`: writes the request's input until the channel closes.
struct InputWriter {
    task: JoinHandle<()>,
    failed: Option<oneshot::Receiver<AdbError>>,
    closed: Arc<AtomicBool>,
}

impl Drop for InputWriter {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl<R: AsyncChannelRequest> ChannelStream<R> {
    pub(crate) fn start(
        factory: Arc<SocketFactory>,
        addr: SocketAddr,
        host_name: Option<String>,
        logging: Option<AdamLogging>,
        idle_override: Option<i64>,
        request: R,
        serial: Option<String>,
    ) -> Self {
        Self {
            pending: VecDeque::new(),
            state: State::Pulling(Box::pin(open(
                factory,
                addr,
                host_name,
                logging,
                idle_override,
                request,
                serial,
            ))),
        }
    }

    /// The next item; `None` after the last one. After an error, the stream is over.
    pub async fn next(&mut self) -> Option<Result<R::Item, AdbError>> {
        poll_fn(|cx| self.poll_next(cx)).await
    }

    pub fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<R::Item, AdbError>>> {
        loop {
            if let Some(item) = self.pending.pop_front() {
                return Poll::Ready(Some(Ok(item)));
            }
            match &mut self.state {
                State::Done => return Poll::Ready(None),
                State::Failed(_) => {
                    if let State::Failed(e) = std::mem::replace(&mut self.state, State::Done) {
                        return Poll::Ready(Some(Err(e)));
                    }
                }
                State::Pulling(pull) => match pull.as_mut().poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Step::Continue(session, items)) => {
                        self.pending.extend(items);
                        self.state = State::Pulling(Box::pin(read(session)));
                    }
                    Poll::Ready(Step::Finished(items, error)) => {
                        self.pending.extend(items);
                        self.state = match error {
                            Some(e) => State::Failed(e),
                            None => State::Done,
                        };
                    }
                },
            }
        }
    }
}

/// Connect, switch transport, handshake and start the input writer, then the first read.
async fn open<R: AsyncChannelRequest>(
    factory: Arc<SocketFactory>,
    addr: SocketAddr,
    host_name: Option<String>,
    logging: Option<AdamLogging>,
    idle_override: Option<i64>,
    mut request: R,
    serial: Option<String>,
) -> Step<R> {
    // A connect failure happens before Kotlin's try, so the request is not closed.
    let mut socket = match super::connect(
        &factory,
        addr,
        host_name.as_deref(),
        idle_override.or(request.socket_idle_timeout()),
        logging.clone(),
    )
    .await
    {
        Ok(socket) => socket,
        Err(e) => return Step::Finished(VecDeque::new(), Some(e)),
    };
    let handshake = async {
        if let Some(serial) = &serial {
            SetDeviceRequest::new(serial).handshake(&mut socket).await?;
        }
        request.handshake(&mut socket).await
    }
    .await;
    let mut session = Box::new(Session {
        request,
        socket,
        writer: None,
        logging,
    });
    if let Err(e) = handshake {
        return finish(session, VecDeque::new(), Some(e)).await;
    }
    session.writer = session
        .request
        .take_channel()
        .map(|channel| spawn_writer::<R>(channel, &session.socket));
    read(session).await
}

fn spawn_writer<R: AsyncChannelRequest>(
    mut channel: tokio::sync::mpsc::Receiver<R::Input>,
    socket: &Socket,
) -> InputWriter {
    let writer = socket.writer();
    let closed = Arc::new(AtomicBool::new(false));
    let (failed_tx, failed) = oneshot::channel();
    let task_closed = closed.clone();
    let task = tokio::spawn(async move {
        while let Some(element) = channel.recv().await {
            if writer.is_closed_for_write() {
                continue;
            }
            if let Err(e) = R::write_element(element, &writer).await {
                // A failing child fails the producer in Kotlin; the reader picks this up.
                let _ = failed_tx.send(e);
                return;
            }
        }
        task_closed.store(true, Ordering::Release);
    });
    InputWriter {
        task,
        failed: Some(failed),
        closed,
    }
}

/// One turn of Kotlin's `while (true)` loop.
async fn read<R: AsyncChannelRequest>(mut session: Box<Session<R>>) -> Step<R> {
    let mut items = VecDeque::new();
    let input_closed = session
        .writer
        .as_ref()
        .is_some_and(|w| w.closed.load(Ordering::Acquire));
    if input_closed || session.socket.is_closed_for_read() {
        return finish(session, items, None).await;
    }
    let Session {
        request,
        socket,
        writer,
        ..
    } = &mut *session;
    let result = match writer {
        Some(writer) => {
            tokio::select! {
                biased;
                e = writer_failure(writer) => Err(e),
                r = request.read_element(socket, &mut items) => r,
            }
        }
        None => request.read_element(socket, &mut items).await,
    };
    match result {
        Ok(false) => Step::Continue(session, items),
        Ok(true) => finish(session, items, None).await,
        Err(e) => finish(session, items, Some(e)).await,
    }
}

/// Resolves with the writer's error; never resolves if it ends without one. A writer that panicked (a
/// `write_element` bug) drops its sender without closing, and fails the stream like an exception in
/// Kotlin's `backChannelJob` would, rather than leaving it to read on with nobody writing.
async fn writer_failure(writer: &mut InputWriter) -> AdbError {
    if let Some(rx) = writer.failed.as_mut() {
        match rx.await {
            Ok(e) => return e,
            Err(_) if !writer.closed.load(Ordering::Acquire) => {
                writer.failed = None;
                return AdbError::IllegalState("The input writer of the request panicked or was cancelled".into());
            }
            Err(_) => {}
        }
    }
    writer.failed = None;
    std::future::pending().await
}

/// Kotlin's `finally`: `request.close(channel)`, then the writer is cancelled; then the socket's `use {}`
/// close, whose failure ends a stream that had not failed already.
async fn finish<R: AsyncChannelRequest>(
    mut session: Box<Session<R>>,
    mut items: VecDeque<R::Item>,
    error: Option<AdbError>,
) -> Step<R> {
    if let Err(e) = session.request.close(&mut items).await
        && let Some(logging) = &session.logging
    {
        logging.log(CLIENT_LOGGER, Level::Debug, Some(&e), || {
            "Exception during cleanup. Ignoring".into()
        });
    }
    let error = error.or_else(|| session.socket.close_result().err());
    drop(session);
    Step::Finished(items, error)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::sync::mpsc;

    use super::*;
    use crate::client::request::create_base_request;
    use crate::client::stub;
    use crate::client::target::Target;
    use crate::client::transport::SocketWriter;

    /// A request whose input writer panics.
    struct PanickingInput(Option<mpsc::Receiver<u8>>);

    impl Request for PanickingInput {
        fn serialize(&self) -> Vec<u8> {
            create_base_request(&Target::NonSpecified, "shell,v2,raw:cat")
        }
    }

    impl AsyncChannelRequest for PanickingInput {
        type Item = Vec<u8>;
        type Input = u8;

        fn take_channel(&mut self) -> Option<mpsc::Receiver<u8>> {
            self.0.take()
        }

        async fn read_element(&mut self, socket: &mut Socket, send: &mut VecDeque<Vec<u8>>) -> Result<bool, AdbError> {
            let mut buf = [0u8; 16];
            let n = socket.read_available(&mut buf).await;
            if n < 0 {
                return Ok(true);
            }
            send.push_back(buf[..n as usize].to_vec());
            Ok(false)
        }

        async fn write_element(_element: u8, _socket: &SocketWriter) -> Result<(), AdbError> {
            panic!("write_element bug");
        }
    }

    /// R1-wonder-6: a panicking `write_element` fails the stream instead of leaving it reading with no
    /// writer.
    #[tokio::test]
    async fn a_panicking_input_writer_fails_the_stream() {
        let server = stub::session(|mut s| async move {
            s.expect_cmd("shell,v2,raw:cat").await;
            s.accept().await;
            // Silent until the client goes away.
            s.discard().await;
        })
        .await;
        let (tx, rx) = mpsc::channel(1);
        tx.send(1).await.unwrap();
        let mut stream = server.client().execute_channel(PanickingInput(Some(rx)), None).unwrap();
        let next = tokio::time::timeout(Duration::from_secs(60), stream.next()).await;
        assert!(
            matches!(next, Ok(Some(Err(AdbError::IllegalState(_))))),
            "{:?}",
            next.map(|r| r.map(|r| r.map(|_| ())))
        );
        drop(stream);
        server.finish().await;
    }
}
