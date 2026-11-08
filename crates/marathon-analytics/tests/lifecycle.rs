//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/external/influx/InfluxDbProvider.kt
//! and external backend client lifecycle (JVM probes and deterministic schedules).
mod support;
use marathon_analytics::{BackendClients, influx::*, influx2::*};
use marathon_core::{analytics::*, error::*, executor::shared::failure, model::TestStatus, runtime::*};
use std::sync::Arc;
use support::*;
use tokio::sync::{mpsc, oneshot};
struct Clock {
    ticks: mpsc::UnboundedSender<oneshot::Sender<()>>,
}
impl VendorClock for Clock {
    fn delay(&self, millis: i64, cancel: Arc<dyn CancellationSignal>) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            assert_eq!(millis, 1000);
            let (tx, rx) = oneshot::channel();
            self.ticks
                .send(tx)
                .map_err(|_| VendorError::Other(failure("test.Clock", "tick receiver closed")))?;
            tokio::select! {biased;reason=cancel.cancelled()=>Err(VendorError::Cancelled(reason)),value=rx=>value.map_err(|_|VendorError::Other(failure("test.Clock","tick dropped")))}
        })
    }
    fn with_timeout<'a>(
        &'a self,
        parent: Arc<dyn CancellationSignal>,
        _: i64,
        body: VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, ()> {
        body(parent)
    }
    fn with_timeout_or_null<'a>(
        &'a self,
        parent: Arc<dyn CancellationSignal>,
        _: i64,
        body: VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, Option<()>> {
        Box::pin(async move { body(parent).await.map(Some) })
    }
}
fn controlled(env: &Env) -> (MetricsContext, mpsc::UnboundedReceiver<oneshot::Sender<()>>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut context = env.context.clone();
    context.clock = Arc::new(Clock { ticks: tx });
    (context, rx)
}
#[test]
fn timer_flush_and_close_join_the_owned_worker() {
    run(|env| {
        Box::pin(async move {
            let (peer, seen) = replay("v1-timer").await?;
            let (context, mut ticks) = controlled(&env);
            let client = InfluxClient::connect(&v1(&peer.url), false, context, env.io).await?;
            let tick = ticks
                .recv()
                .await
                .ok_or_else(|| failure("test.Clock", "missing timer"))?;
            InfluxTracker {
                client: client.clone(),
                read_only: false,
            }
            .track_test(&event(TestStatus::Passed, false))
            .await?;
            assert_eq!(*seen.lock().unwrap(), 3);
            assert!(tick.send(()).is_ok());
            peer.wait_requests(4).await;
            let next = ticks
                .recv()
                .await
                .ok_or_else(|| failure("test.Clock", "missing rescheduled timer"))?;
            client.shutdown().await?;
            assert!(next.send(()).is_err());
            assert_eq!(*seen.lock().unwrap(), 4);
            peer.shutdown().await?;
            Ok(())
        })
    });
}
#[test]
fn threshold_flushes_1000_points_without_a_timer_tick() {
    run(|env| {
        Box::pin(async move {
            let (peer, seen) = replay("v1-threshold").await?;
            let (context, mut ticks) = controlled(&env);
            let client = InfluxClient::connect(&v1(&peer.url), false, context, env.io).await?;
            let tick = ticks
                .recv()
                .await
                .ok_or_else(|| failure("test.Clock", "missing timer"))?;
            let tracker = InfluxTracker {
                client: client.clone(),
                read_only: false,
            };
            for _ in 0..1000 {
                tracker.track_test(&event(TestStatus::Passed, false)).await?;
            }
            peer.wait_requests(4).await;
            client.shutdown().await?;
            assert!(tick.send(()).is_err());
            assert_eq!(*seen.lock().unwrap(), 4);
            peer.shutdown().await?;
            Ok(())
        })
    });
}
#[test]
fn batch_retries_and_permanent_failures_match_the_jvm() {
    run(|env| {
        Box::pin(async move {
            for scenario in ["v1-retry", "v1-permanent"] {
                let (peer, seen) = replay(scenario).await?;
                let (context, _ticks) = controlled(&env);
                let client = InfluxClient::connect(&v1(&peer.url), false, context, env.io.clone()).await?;
                InfluxTracker {
                    client: client.clone(),
                    read_only: false,
                }
                .track_test(&event(TestStatus::Passed, false))
                .await?;
                client.flush().await?;
                client.flush().await?;
                client.shutdown().await?;
                assert_eq!(*seen.lock().unwrap(), exchanges(scenario).len());
                peer.shutdown().await?;
            }
            Ok(())
        })
    });
}
#[test]
fn separate_history_and_tracking_clients_run_setup_twice() {
    run(|env| {
        Box::pin(async move {
            let requests = Arc::new(std::sync::Mutex::new(0));
            let count = requests.clone();
            let peer=Peer::new(move|_|{*count.lock().unwrap()+=1;Reply::ok("{\"results\":[{\"series\":[{\"name\":\"databases\",\"columns\":[\"name\"],\"values\":[[\"db\"]]}]}]}")}).await?;
            let clients = Arc::new(env.clients());
            let config = v1(&peer.url);
            let history = clients.influx(&config, false).await?;
            let factory = BackendClients {
                clients,
                config: marathon_config::AnalyticsConfiguration::InfluxDb(config),
                debug: false,
            };
            let tracker = factory
                .create(BackendRequest::Influx {
                    url: peer.url.clone(),
                })
                .await?
                .map_err(OperationError::Failed)?
                .ok_or_else(|| failure("test.Factory", "missing tracker"))?;
            assert_eq!(*requests.lock().unwrap(), 2);
            history.close().await?;
            tracker.close().await?;
            peer.shutdown().await?;
            Ok(())
        })
    });
}
#[test]
fn close_is_awaited_during_an_in_flight_query() {
    run(|env| {
        Box::pin(async move {
            // The peer releases the response only after the close future has polled.
            use tokio::{
                io::{AsyncReadExt, AsyncWriteExt},
                net::TcpListener,
            };
            let listener = io(TcpListener::bind("127.0.0.1:0").await)?;
            let address = io(listener.local_addr())?;
            let (entered, started) = oneshot::channel();
            let (release, released) = oneshot::channel();
            let server = tokio::spawn(async move {
                // Setup requests precede the gated query.
                let mut entered = Some(entered);
                let mut released = Some(released);
                for index in 0..3 {
                    let (mut socket, _) = io(listener.accept().await)?;
                    let mut buffer = [0; 8192];
                    let _ = io(socket.read(&mut buffer).await)?;
                    let body = match index {
                        0 => "{\"orgs\":[{\"name\":\"org\",\"id\":\"id1\"}]}",
                        1 => "{\"buckets\":[{\"id\":\"bucket1\"}]}",
                        _ => "",
                    };
                    if index == 2 {
                        let _ = entered.take().unwrap().send(());
                        released
                            .take()
                            .unwrap()
                            .await
                            .map_err(|_| failure("test.Barrier", "release dropped"))?;
                    }
                    io(socket
                        .write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                            .as_bytes(),
                        )
                        .await)?;
                }
                Ok::<_, OperationError>(())
            });
            let client = Influx2Client::connect(&v2(&format!("http://{address}")), false, env.context).await?;
            let query_client = client.clone();
            let query = tokio::spawn(async move {
                use marathon_core::analytics::remote::influx2::FluxQuery;
                query_client.query(&"bad".into()).await
            });
            started
                .await
                .map_err(|_| failure("test.Barrier", "query did not start"))?;
            let close = client.shutdown();
            tokio::pin!(close);
            assert!(futures_util::poll!(&mut close).is_pending());
            let _ = release.send(());
            query.await.map_err(|e| failure("test.Query", &e.to_string()))??;
            close.await?;
            server.await.map_err(|e| failure("test.Peer", &e.to_string()))??;
            Ok(())
        })
    });
}
#[test]
fn jcompat_wire_format_helpers_match_jvm_values() {
    for row in observations("duration-format") {
        assert_eq!(
            jcompat::jnum::influx_duration(row["value"].as_i64().unwrap()),
            row["text"]
        );
    }
    assert_eq!(jcompat::net::form_encode("a b+*~é"), "a+b%2B*%7E%C3%A9");
    assert_eq!(
        jcompat::jtime::instant_to_epoch_milli(jcompat::jtime::instant_of(-2, 999_000_000).unwrap()),
        Some(-1001)
    );
}

#[test]
fn instant_precision_and_custom_logger_names_match_the_jvm() {
    for row in observations("v2-instant") {
        let instant = jcompat::jtime::parse_instant(row["instant"].as_str().unwrap()).unwrap();
        let timestamp = jcompat::jtime::influx_epoch_millis(instant);
        assert_eq!(format!("tests duration=1.0 {timestamp}"), row["line"]);
    }
    for row in observations("epoch-millis") {
        let instant = jcompat::jtime::parse_instant(row["instant"].as_str().unwrap()).unwrap();
        assert_eq!(jcompat::jtime::instant_to_epoch_milli(instant), row["millis"].as_i64());
    }
    assert!(
        jcompat::jtime::instant_to_epoch_milli(jcompat::jtime::parse_instant("-1000000000-01-01T00:00:00Z").unwrap())
            .is_none()
    );
    let logs = run(|env| {
        Box::pin(async move {
            let (peer, _) = replay("v2-missing").await?;
            assert!(
                Influx2Client::connect(&v2(&peer.url), false, env.context)
                    .await
                    .is_err()
            );
            peer.shutdown().await?;
            Ok(())
        })
    });
    assert!(logs.is_empty());
}

#[test]
fn a_cancelled_run_can_still_drain_the_v1_batch_on_close() {
    run(|env| {
        Box::pin(async move {
            let (peer, seen) = replay("v1-timer").await?;
            let (context, _ticks) = controlled(&env);
            let signal = marathon_core::executor::Signal::new();
            let mut context = context;
            context.cancellation = signal.clone();
            let client = InfluxClient::connect(&v1(&peer.url), false, context, env.io).await?;
            InfluxTracker {
                client: client.clone(),
                read_only: false,
            }
            .track_test(&event(TestStatus::Passed, false))
            .await?;
            signal.cancel(marathon_core::executor::shared::requested("run cancelled"));
            client.shutdown().await?;
            assert_eq!(*seen.lock().unwrap(), 4);
            peer.shutdown().await?;
            Ok(())
        })
    });
}
