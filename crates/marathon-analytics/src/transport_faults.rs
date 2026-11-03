//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/external/influx/InfluxDbProvider.kt
//! and external/influx2/InfluxDb2Provider.kt (fault behaviour of the production v1/v2 query transports).
//!
//! The JVM recorded EOF, reset and slow peers with 100 ms OkHttp limits; these
//! replay the same peers through the production wrappers with shortened limits.
use crate::{http::tests::context, http::*, influx, influx2};
use marathon_core::error::*;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{io::AsyncReadExt, io::AsyncWriteExt, net::TcpListener, sync::Notify};

fn observation(scenario: &str) -> serde_json::Value {
    include_str!("../../../fixtures/oracle/analytics-clients/observations.jsonl")
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|row| row["scenario"] == scenario)
        .unwrap_or_else(|| panic!("missing JVM observation {scenario}"))
}
fn io<T>(result: std::io::Result<T>) -> OperationResult<T> {
    result.map_err(|e| network_error(&e).into())
}

/// One faulty connection, then a window, open until the client returns, in which a transport retry would reconnect.
async fn fault(backend: &str, mode: &'static str) -> OperationResult<()> {
    let listener = io(TcpListener::bind("127.0.0.1:0").await)?;
    let url = format!("http://{}", io(listener.local_addr())?);
    let accepted = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(Notify::new());
    let (count, released) = (accepted.clone(), release.clone());
    let peer = tokio::spawn(async move {
        let (mut socket, _) = io(listener.accept().await)?;
        count.fetch_add(1, Ordering::SeqCst);
        let _ = io(socket.read(&mut [0; 8192]).await)?;
        match mode {
            "reset" => {
                #[allow(deprecated)]
                let linger = socket.set_linger(Some(Duration::ZERO));
                io(linger)?;
            }
            "slow" => released.notified().await,
            _ => {}
        }
        drop(socket);
        // A retry would reconnect before the client returns, so the window stays open until then
        // rather than for a fixed time a loaded host could outlast.
        let retry = match mode {
            "slow" => None,
            _ => tokio::select! {
                accept = listener.accept() => Some(accept),
                () = released.notified() => None,
            },
        };
        if let Some(accept) = retry {
            let (mut socket, _) = io(accept)?;
            count.fetch_add(1, Ordering::SeqCst);
            let _ = io(socket.read(&mut [0; 8192]).await)?;
            io(socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await)?;
        }
        Ok::<_, OperationError>(())
    });
    let mut http = Http::new(&url.into(), Some("Token dummy-token".into()), context(), false)?;
    http.set_idle_timeouts(Duration::from_millis(100));
    let result = if backend == "v1" {
        influx::request(&http, "GET", "query?db=db&q=SELECT+x+FROM+tests", None, String::new()).await
    } else {
        influx2::api(
            &http,
            "POST",
            "api/v2/query?org=org",
            Some("application/json"),
            None,
            "{}".into(),
        )
        .await
    };
    release.notify_one();
    peer.await
        .map_err(|e| OperationError::from(failure_of(&e.to_string())))??;
    let scenario = format!("fault-{backend}-{mode}");
    let expected = observation(&scenario);
    let Err(OperationError::Failed(error)) = result else {
        panic!("{scenario}: fault accepted")
    };
    assert_eq!(error.class.to_string_lossy(), expected["class"], "{scenario}");
    assert_eq!(
        error.cause.as_ref().map(|c| c.class.to_string_lossy()),
        expected["cause"].as_str().map(str::to_owned),
        "{scenario}"
    );
    assert_eq!(
        accepted.load(Ordering::SeqCst) as u64,
        observation(&format!("{scenario}-count"))["connections"]
            .as_u64()
            .unwrap_or(0),
        "{scenario}"
    );
    Ok(())
}
fn failure_of(message: &str) -> Failure {
    marathon_core::executor::shared::failure("test.Peer", message)
}

#[tokio::test]
async fn eof_reset_and_slow_peers_match_jvm_wrappers_causes_and_connection_counts() {
    for backend in ["v1", "v2"] {
        for mode in ["eof", "reset", "slow"] {
            let result = tokio::time::timeout(Duration::from_secs(60), fault(backend, mode)).await;
            assert!(matches!(result, Ok(Ok(()))), "fault-{backend}-{mode}: {result:?}");
        }
    }
}

#[tokio::test]
async fn refused_v2_query_wraps_connect_exception() {
    let expected = observation("v2-refused-query");
    let http = Http::new(
        &"http://127.0.0.1:1".into(),
        Some("Token dummy-token".into()),
        context(),
        false,
    )
    .unwrap_or_else(|e| panic!("{e:?}"));
    let Err(OperationError::Failed(error)) = influx2::api(
        &http,
        "POST",
        "api/v2/query?org=org",
        Some("application/json"),
        None,
        "{}".into(),
    )
    .await
    else {
        panic!("refused query accepted")
    };
    assert_eq!(error.class.to_string_lossy(), expected["class"]);
    assert_eq!(
        error.cause.as_ref().map(|c| c.class.to_string_lossy()),
        expected["cause"].as_str().map(str::to_owned)
    );
}
