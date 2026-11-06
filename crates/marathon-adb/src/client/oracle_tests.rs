//! The JVM probe cases (`tools/oracle/adb-client-probes`) replayed against the Rust client, compared with
//! what adam 0.5.8 did (`fixtures/oracle/adb-client-probes.jsonl`). The two cases where 6.1 deviates on
//! purpose (D11, P5) check the recorded Kotlin value and the Rust one side by side.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use jcompat::logging::Level;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use super::device::{AsyncDeviceMonitorRequest, ListDevicesRequest};
use super::feature::Feature;
use super::log::capture;
use super::misc::GetAdbServerVersionRequest;
use super::shell::{v1, v2};
use super::stub::{self, StubSession};
use super::transport::{DEFAULT_CONNECT_TIMEOUT_MS, SocketFactory};
use super::{AdbError, AndroidDebugBridgeClient, AsyncCompatShellCommandRequest, JavaThrowable, Request};

fn kotlin() -> HashMap<String, String> {
    include_str!("../../../../fixtures/oracle/adb-client-probes.jsonl")
        .lines()
        .map(|line| {
            let row: serde_json::Value = serde_json::from_str(line).unwrap();
            (
                row["case"].as_str().unwrap().to_string(),
                row["value"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// The probe's `"${javaClass.name}: ${message}"`, which prints a missing message as `null`.
fn describe(e: &AdbError) -> String {
    describe_throwable(&e.to_throwable())
}

fn describe_throwable(t: &JavaThrowable) -> String {
    format!("{}: {}", t.class, t.message.as_deref().unwrap_or("null"))
}

/// The probe's `withTimeoutOrNull` rows.
fn or_null<T: ToString>(value: Option<T>) -> String {
    value.map_or("null".to_string(), |v| v.to_string())
}

async fn transport_and_shell(s: &mut StubSession, shell: &str) {
    s.expect_cmd("host:transport:s").await;
    s.accept().await;
    s.expect_cmd(shell).await;
    s.accept().await;
}

#[tokio::test]
async fn v2_exit_codes() {
    let kotlin = kotlin();
    for code in [255u8, 128] {
        let server = stub::session(move |mut s| async move {
            transport_and_shell(&mut s, "shell,v2,raw:x").await;
            s.respond(&[3, 1, 0, 0, 0, code]).await;
        })
        .await;
        let mut stream = server
            .client()
            .execute_channel(v2::ChanneledShellCommandRequest::new("x", None), Some("s"))
            .unwrap();
        let mut codes = Vec::new();
        while let Some(chunk) = stream.next().await {
            codes.push(chunk.unwrap().exit_code.map_or("null".to_string(), |c| c.to_string()));
        }
        assert_eq!(codes.join(","), kotlin[&format!("v2-exit-{code}")]);
        server.finish().await;
    }
}

#[tokio::test]
async fn handshake_eof_and_its_warning() {
    let kotlin = kotlin();
    let server = stub::session(|mut s| async move {
        s.expect_cmd("host:version").await;
        s.respond(b"OK").await;
    })
    .await;
    let (logging, events) = capture::logging(true);
    let error = server
        .client_with(Some(logging))
        .execute(GetAdbServerVersionRequest, None)
        .await
        .unwrap_err();
    assert_eq!(describe(&error), kotlin["handshake-eof"]);
    let events = events.lock().unwrap().clone();
    let (logger, level, message) = &events[0];
    assert_eq!(*level, Level::Warn);
    assert_eq!(
        format!("WARN {message}"),
        kotlin[&format!("log:{logger}")],
        "logger {logger}"
    );
    server.finish().await;
}

#[tokio::test]
async fn track_devices_eof() {
    let kotlin = kotlin();
    let server = stub::session(|mut s| async move {
        s.expect_cmd("host:track-devices").await;
        s.accept().await;
        s.respond(b"000Eserial\tdevice\n").await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    })
    .await;
    let mut stream = server
        .client()
        .execute_channel(AsyncDeviceMonitorRequest, None)
        .unwrap();
    let first = stream.next().await.unwrap().unwrap();
    assert_eq!(
        format!("{}={:?}", first[0].serial, first[0].state).to_uppercase(),
        kotlin["track-devices-first"].to_uppercase()
    );
    let error = stream.next().await.unwrap().unwrap_err();
    assert_eq!(describe(&error), kotlin["track-devices-eof"]);
    server.finish().await;
}

#[tokio::test]
async fn devices_line_without_tab() {
    let kotlin = kotlin();
    let server = stub::session(|mut s| async move {
        s.expect_cmd("host:devices").await;
        s.accept().await;
        s.respond(b"0010a\tdevice\nno-tab\n").await;
    })
    .await;
    let error = server.client().execute(ListDevicesRequest, None).await.unwrap_err();
    assert_eq!(describe(&error), kotlin["devices-line-without-tab"]);
    server.finish().await;
}

#[tokio::test]
async fn idle_timeout_unit() {
    let kotlin = kotlin();
    // Kotlin's 1 "ms" lasted one second (N12), so the recorded rows are what 1000 ms does now (D21).
    for delay in [200u64, 1500] {
        let server = stub::session(move |mut s| async move {
            transport_and_shell(&mut s, "shell:x;echo x$?").await;
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            // The client may have closed already.
            let _ = s.try_respond(b"late\nx0").await;
        })
        .await;
        let factory = Arc::new(SocketFactory::new(DEFAULT_CONNECT_TIMEOUT_MS, 1000));
        let client = AndroidDebugBridgeClient::new(server.addr().ip(), server.addr().port(), factory, None);
        let started = Instant::now();
        let outcome = match client.execute(v1::ShellCommandRequest::new("x"), Some("s")).await {
            Ok(r) => format!("output={} exit={}", r.output(), r.exit_code),
            Err(e) => describe(&e),
        };
        let tenths = started.elapsed().as_millis() / 100;
        let expected = &kotlin[&format!("idle-1ms-delay-{delay}")];
        let (expected_outcome, expected_after) = expected.rsplit_once(" after>=").unwrap();
        assert_eq!(outcome, expected_outcome);
        // Kotlin's row floors the elapsed time to tenths of a second (1.0-1.3 s all read 1.0 s).
        let expected_tenths: u128 = (expected_after.trim_end_matches('s').parse::<f64>().unwrap() * 10.0) as u128;
        assert!(
            tenths >= expected_tenths && tenths <= expected_tenths + 3,
            "{tenths} vs {expected}"
        );
        server.finish().await;
    }
}

#[tokio::test]
async fn v1_split_character_is_decoded_whole_d11() {
    let kotlin = kotlin();
    let server = stub::session(|mut s| async move {
        transport_and_shell(&mut s, "shell:x").await;
        let bytes = "aжb".as_bytes();
        s.respond(&bytes[..2]).await;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        s.respond(&bytes[2..]).await;
    })
    .await;
    let mut stream = server
        .client()
        .execute_channel(v1::ChanneledShellCommandRequest::new("x"), Some("s"))
        .unwrap();
    let mut parts = Vec::new();
    while let Some(part) = stream.next().await {
        parts.push(part.unwrap());
    }
    // adam decodes each read alone; across reads the character survives.
    assert_eq!(kotlin["v1-split-utf8"], "a\u{fffd}|\u{fffd}b");
    assert_eq!(parts.join("|"), "a|жb");
    server.finish().await;
}

#[test]
fn oversized_request_header_p5() {
    let kotlin = kotlin();
    let request = v1::ShellCommandRequest::new("y".repeat(0x10000));
    let bytes = request.serialize();
    // adam overwrites four of its five header digits, sending a header that claims 0x1001 bytes; the Rust
    // client refuses such a request before writing it (see android_debug_bridge_client_test).
    assert_eq!(kotlin["oversized-header"], "1001Fs");
    assert_eq!(kotlin["oversized-length"], bytes.len().to_string());
    assert_eq!(&bytes[..6], b"1000Fs");
}

#[tokio::test]
async fn close() {
    let kotlin = kotlin();
    let track = stub::session(|mut s| async move {
        s.expect_cmd("host:track-devices").await;
        s.accept().await;
        s.respond(b"000Eserial\tdevice\n").await;
        tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
    })
    .await;
    let logcat = stub::session(|mut s| async move {
        transport_and_shell(&mut s, "shell:logcat").await;
        s.respond(b"line\n").await;
        tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
    })
    .await;
    let factory = Arc::new(SocketFactory::new(DEFAULT_CONNECT_TIMEOUT_MS, 30_000));
    let client = |port| AndroidDebugBridgeClient::new(track.addr().ip(), port, factory.clone(), None);
    let mut track_stream = client(track.addr().port())
        .execute_channel(AsyncDeviceMonitorRequest, None)
        .unwrap();
    track_stream.next().await.unwrap().unwrap();
    let mut logcat_stream = client(logcat.addr().port())
        .execute_channel(v1::ChanneledShellCommandRequest::new("logcat"), Some("s"))
        .unwrap();
    logcat_stream.next().await.unwrap().unwrap();
    client(track.addr().port()).close();
    let error = track_stream.next().await.unwrap().unwrap_err();
    assert_eq!(describe(&error), kotlin["close-track-devices"]);
    let error = logcat_stream.next().await.unwrap().unwrap_err();
    assert_eq!(describe(&error), kotlin["close-v1-stream"]);
    let error = client(track.addr().port())
        .execute(GetAdbServerVersionRequest, None)
        .await
        .unwrap_err();
    assert_eq!(describe(&error), kotlin["close-new-request"]);
    assert_eq!(
        error.is_exception().to_string(),
        kotlin["close-new-request-is-Exception"]
    );
    drop((track_stream, logcat_stream));
}

#[tokio::test]
async fn connect_refused() {
    let kotlin = kotlin();
    let port = crate::server::closed_port([127, 0, 0, 1].into()).unwrap();
    let factory = Arc::new(SocketFactory::new(DEFAULT_CONNECT_TIMEOUT_MS, 30_000));
    let client = AndroidDebugBridgeClient::new([127, 0, 0, 1].into(), port, factory.clone(), None);
    let error = client.execute(GetAdbServerVersionRequest, None).await.unwrap_err();
    assert_eq!(
        describe(&error).replace(&format!(":{port}"), ":<port>"),
        kotlin["connect-refused"]
    );
    assert_eq!(
        error.is_connect_exception().to_string(),
        kotlin["connect-refused-is-ConnectException"]
    );
    // Kotlin resolves "localhost" itself; 6.5 hands over the address and the configured name.
    // R1-sol-7: the whole throwable, with netty's cause.
    let refused = kotlin["connect-refused"].replace(":<port>", &format!(":{port}"));
    let (class, message) = refused.split_once(": ").unwrap();
    let (cause_class, cause_message) = kotlin["connect-refused-cause"].split_once(": ").unwrap().to_owned();
    assert_eq!(
        error.to_throwable(),
        JavaThrowable {
            class: error.java_class(),
            message: Some(message.to_string()),
            cause: Some(Box::new(JavaThrowable {
                class: "java.net.ConnectException",
                message: Some(cause_message.to_string()),
                cause: None,
            })),
        }
    );
    assert_eq!((error.java_class(), cause_class), (class, "java.net.ConnectException"));
    let named = AndroidDebugBridgeClient::new([127, 0, 0, 1].into(), port, factory, None).with_host_name("localhost");
    assert_eq!(named.host_name(), Some("localhost"));
    let error = named.execute(GetAdbServerVersionRequest, None).await.unwrap_err();
    assert_eq!(
        describe(&error).replace(&format!(":{port}"), ":<port>"),
        kotlin["connect-refused-named"]
    );
}

/// R1-sol-6: Java writes an IPv6 address in full, in brackets.
#[tokio::test]
async fn connect_refused_ipv6() {
    let kotlin = kotlin();
    let Ok(port) = crate::server::closed_port("::1".parse().unwrap()) else {
        eprintln!("no IPv6 loopback here; skipped");
        return;
    };
    let factory = Arc::new(SocketFactory::new(DEFAULT_CONNECT_TIMEOUT_MS, 30_000));
    let client = AndroidDebugBridgeClient::new("::1".parse().unwrap(), port, factory, None).with_host_name("::1");
    let error = client.execute(GetAdbServerVersionRequest, None).await.unwrap_err();
    assert_eq!(
        describe(&error).replace(&format!(":{port}"), ":<port>"),
        kotlin["connect-refused-ipv6"]
    );
}

/// R1-sol-1: each 16 KiB piece of a v2 packet is an item as soon as it is read, not when the packet is
/// complete. The rest of the packet is held back until the first piece is out.
#[tokio::test]
async fn v2_partial_packet() {
    let kotlin = kotlin();
    let (release, released) = oneshot::channel::<()>();
    let server = stub::session(|mut s| async move {
        transport_and_shell(&mut s, "shell,v2,raw:x").await;
        s.respond(&[1, 0, 128, 0, 0]).await;
        s.respond(&[b'x'; 16384]).await;
        let _ = released.await;
        s.respond(&[b'y'; 16384]).await;
        s.respond(&[3, 1, 0, 0, 0, 0]).await;
    })
    .await;
    let mut stream = server
        .client()
        .execute_channel(v2::ChanneledShellCommandRequest::new("x", None), Some("s"))
        .unwrap();
    let first = tokio::time::timeout(Duration::from_secs(60), stream.next()).await.ok();
    let first = first
        .flatten()
        .map(|chunk| chunk.unwrap().stdout.unwrap_or_default().len());
    assert_eq!(or_null(first), kotlin["v2-partial-packet"]);
    let _ = release.send(());
    let mut rest = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.unwrap();
        rest.push(match chunk.stdout {
            Some(stdout) => stdout.len().to_string(),
            None => format!("exit={}", or_null(chunk.exit_code)),
        });
    }
    assert_eq!(rest.join(","), kotlin["v2-partial-packet-rest"]);
    server.finish().await;
}

/// R1-sol-2: the idle timer fails a write the server never takes in.
#[tokio::test]
async fn idle_blocked_write() {
    let kotlin = kotlin();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let factory = SocketFactory::new(DEFAULT_CONNECT_TIMEOUT_MS, 1000);
    let socket = factory
        .tcp(listener.local_addr().unwrap(), None, None, None)
        .await
        .unwrap();
    // Accepted, never read.
    let (_peer, _) = listener.accept().await.unwrap();
    let bytes = vec![0u8; 32 << 20];
    let outcome = tokio::time::timeout(Duration::from_secs(60), socket.write_fully(&bytes))
        .await
        .ok()
        .map(|r| r.map_or_else(|e| describe(&e), |()| "ok".to_string()));
    assert_eq!(or_null(outcome), kotlin["idle-blocked-write"]);
}

/// R1-sol-3: closing the factory closes an idle connection; its server reads EOF (-1) at once.
#[tokio::test]
async fn close_peer_eof() {
    let kotlin = kotlin();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let factory = SocketFactory::new(DEFAULT_CONNECT_TIMEOUT_MS, 0);
    let socket = factory
        .tcp(listener.local_addr().unwrap(), None, None, None)
        .await
        .unwrap();
    let (mut peer, _) = listener.accept().await.unwrap();
    factory.close();
    let mut byte = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(60), peer.read(&mut byte)).await;
    let read = read.ok().map(|r| match r {
        Ok(0) | Err(_) => -1,
        Ok(_) => i32::from(byte[0]),
    });
    assert_eq!(or_null(read), kotlin["close-peer-eof"]);
    drop(socket);
}

/// R1-sol-5: adam's vert.x does not exist before the first request, so closing it then does nothing.
#[tokio::test]
async fn close_before_first_request() {
    let kotlin = kotlin();
    let server = stub::session(|mut s| async move {
        s.expect_cmd("host:version").await;
        s.respond(b"OKAY00040029").await;
    })
    .await;
    let client = server.client();
    client.close();
    let outcome = match client.execute(GetAdbServerVersionRequest, None).await {
        Ok(version) => version.to_string(),
        Err(e) => describe(&e),
    };
    assert_eq!(outcome, kotlin["close-before-first-request"]);
    server.finish().await;
}

/// A compat request whose `close` sends 7 once released.
struct SlowClose {
    started: Option<oneshot::Sender<()>>,
    release: Option<oneshot::Receiver<()>>,
}

impl AsyncCompatShellCommandRequest for SlowClose {
    type Item = i32;

    fn cmd(&self) -> String {
        "x".into()
    }

    fn supported_features(&self) -> &[Feature] {
        &[]
    }

    fn convert_chunk(&mut self, _chunk: v2::ShellCommandResultChunk) -> Option<i32> {
        None
    }

    async fn close(&mut self, send: &mut VecDeque<i32>) {
        if let Some(started) = self.started.take() {
            let _ = started.send(());
        }
        if let Some(release) = self.release.take() {
            let _ = release.await;
        }
        send.push_back(7);
    }
}

/// R1-sol-4: a `next()` given up while `close` runs loses neither the cleanup nor its items. Kotlin's
/// first receive times out after 150 ms of a 400 ms close; here the first `next()` is dropped once
/// `close` has started, and `close` finishes only after that.
#[tokio::test]
async fn compat_close_cancelled() {
    let kotlin = kotlin();
    let server = stub::session(|mut s| async move {
        s.expect_cmd("shell:x").await;
        s.accept().await;
    })
    .await;
    let (started_tx, mut started) = oneshot::channel();
    let (release, release_rx) = oneshot::channel();
    let mut stream = server.client().execute_compat(
        SlowClose {
            started: Some(started_tx),
            release: Some(release_rx),
        },
        None,
    );
    let first = tokio::select! {
        item = stream.next() => item.map(|r| r.unwrap()),
        _ = &mut started => None,
    };
    assert_eq!(or_null(first), kotlin["compat-close-cancelled-first"]);
    let _ = release.send(());
    let second = tokio::time::timeout(Duration::from_secs(60), stream.next())
        .await
        .ok()
        .flatten();
    assert_eq!(
        or_null(second.map(|r| r.unwrap())),
        kotlin["compat-close-cancelled-second"]
    );
    assert!(stream.request().is_some());
    assert!(stream.next().await.is_none());
    server.finish().await;
}

#[test]
fn every_probe_case_is_replayed() {
    let replayed = [
        "v2-exit-255",
        "v2-exit-128",
        "log:com.malinskiy.adam.request.Request",
        "handshake-eof",
        "track-devices-first",
        "track-devices-eof",
        "devices-line-without-tab",
        "idle-1ms-delay-200",
        "idle-1ms-delay-1500",
        "v1-split-utf8",
        "oversized-header",
        "oversized-length",
        "close-track-devices",
        "close-v1-stream",
        "close-new-request",
        "close-new-request-is-Exception",
        "connect-refused",
        "connect-refused-is-ConnectException",
        "connect-refused-named",
        "connect-refused-cause",
        "connect-refused-ipv6",
        "v2-partial-packet",
        "v2-partial-packet-rest",
        "idle-blocked-write",
        "close-peer-eof",
        "close-before-first-request",
        "compat-close-cancelled-first",
        "compat-close-cancelled-second",
        // Replayed by `jcompat::net`.
        "inet-socket-address",
    ];
    let mut cases: Vec<_> = kotlin().into_keys().collect();
    cases.sort();
    let mut replayed = replayed.map(String::from).to_vec();
    replayed.sort();
    assert_eq!(cases, replayed);
}
