//! Port of core/src/test/kotlin/com/malinskiy/marathon/analytics/external/graphite/BasicGraphiteClientTest.kt
//! and external backend provider/tracker behavior probes.
use marathon_error::{FailureKind, InjectedKind};
mod support;
use marathon_analytics::{graphite::*, influx::*, influx2::*};
use marathon_core::{
    analytics::{
        remote::{graphite::GraphiteQuery, influx::InfluxQuery, influx2::FluxQuery},
        *,
    },
    error::*,
    executor::{Signal, shared::requested},
    model::TestStatus,
    runtime::*,
};
use std::sync::{Arc, Mutex};
use support::*;

#[derive(Default)]
struct Sender(Mutex<Vec<String>>);
impl DataSender for Sender {
    fn send_data<'a>(
        &'a self,
        _: &'a str,
        _: u16,
        data: String,
        _: Arc<dyn CancellationSignal>,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            self.0.lock().unwrap().push(data);
            Ok(())
        })
    }
}
fn client(sender: Arc<dyn DataSender>) -> BasicGraphiteClient {
    BasicGraphiteClient {
        host: "host".into(),
        port: 2003,
        prefix: None,
        sender,
        cancellation: Signal::new(),
    }
}
#[test]
fn sending_a_single_metric_should_invoke_send_data_once() {
    run(|_| {
        Box::pin(async {
            let sender = Arc::new(Sender::default());
            client(sender.clone())
                .send(GraphiteMetric {
                    metric_name: "metric".into(),
                    metric_value: "1".into(),
                    timestamp: -1001,
                })
                .await?;
            assert_eq!(*sender.0.lock().unwrap(), ["metric 1 -1\n"]);
            Ok(())
        })
    });
}
#[test]
fn sending_multiple_metrics_should_invoke_send_data_once() {
    run(|_| {
        Box::pin(async {
            let sender = Arc::new(Sender::default());
            client(sender.clone())
                .send_all(&[
                    GraphiteMetric {
                        metric_name: "one".into(),
                        metric_value: "1".into(),
                        timestamp: 0,
                    },
                    GraphiteMetric {
                        metric_name: "two".into(),
                        metric_value: "2".into(),
                        timestamp: 2000,
                    },
                ])
                .await?;
            assert_eq!(*sender.0.lock().unwrap(), ["one 1 0\ntwo 2 2\n"]);
            Ok(())
        })
    });
}
#[test]
fn sending_an_empty_metrics_list_should_not_invoke_send_data() {
    run(|_| {
        Box::pin(async {
            let sender = Arc::new(Sender::default());
            client(sender.clone()).send_all(&[]).await?;
            assert!(sender.0.lock().unwrap().is_empty());
            Ok(())
        })
    });
}
#[test]
fn jvm_v1_setup_credentials_statuses_and_close_flush() {
    run(|env| {
        Box::pin(async move {
            for name in ["v1-fresh", "v1-existing", "v1-partial", "v1-auth"] {
                let (peer, seen) = replay(name).await?;
                let mut config = v1(&peer.url);
                if name == "v1-auth" || name == "v1-partial" {
                    config.user = "user".into();
                }
                if name == "v1-auth" {
                    config.password = "pass".into();
                }
                let client =
                    InfluxClient::connect(&config, name == "v1-auth", env.context.clone(), env.io.clone()).await?;
                let tracker = InfluxTracker {
                    client: client.clone(),
                    read_only: false,
                };
                for status in statuses() {
                    tracker.track_test(&event(status, true)).await?;
                }
                InfluxTracker {
                    client: client.clone(),
                    read_only: true,
                }
                .track_test(&event(TestStatus::Passed, false))
                .await?;
                client.shutdown().await?;
                client.shutdown().await?;
                assert_eq!(*seen.lock().unwrap(), exchanges(name).len());
                peer.shutdown().await?;
            }
            Ok(())
        })
    });
}
#[test]
fn jvm_v2_setup_statuses_and_exact_custom_logs() {
    let logs = run(|env| {
        Box::pin(async move {
            for name in ["v2-fresh", "v2-existing", "v2-duplicate", "v2-missing"] {
                let (peer, seen) = replay(name).await?;
                let result = Influx2Client::connect(&v2(&peer.url), name == "v2-duplicate", env.context.clone()).await;
                if name == "v2-missing" {
                    let Err(OperationError::Failed(f)) = result else {
                        panic!("missing organization accepted")
                    };
                    assert_eq!(f.message.unwrap().to_owned(), "No organization with name = org found");
                } else {
                    let client = result?;
                    let tracker = Influx2Tracker::new(client.clone(), false)?;
                    for status in statuses() {
                        tracker.track_test(&event(status, true)).await?;
                    }
                    Influx2Tracker::new(client.clone(), true)?
                        .track_test(&event(TestStatus::Passed, false))
                        .await?;
                    client.shutdown().await?;
                    client.shutdown().await?;
                }
                assert_eq!(*seen.lock().unwrap(), exchanges(name).len());
                peer.shutdown().await?;
            }
            Ok(())
        })
    });
    assert!(
        logs.iter()
            .any(|r| r.message == "Multiple organizations with name = org found. Using first one, id = id1")
    );
    assert!(
        logs.iter()
            .filter(|r| r.logger.ends_with("InfluxDb2Provider"))
            .all(|r| r.logger == "com.malinskiy.marathon.analytics.external.influx2.InfluxDb2Provider")
    );
    assert!(logs.iter().any(|r|r.message=="Creating InfluxDB 2 bucket: orgId = id1, bucket = bucket, retention: RetentionPolicyConfiguration(everySeconds=2592000, shardGroupDurationSeconds=0)"));
}
#[test]
fn jvm_graphite_path_line_order_and_status_gates() {
    run(|_| {
        Box::pin(async {
            let sender = Arc::new(Sender::default());
            let mut client = client(sender.clone());
            client.prefix = Some("a.b".into());
            let tracker = GraphiteTracker {
                client,
                read_only: false,
            };
            for status in statuses() {
                tracker.track_test(&event(status, true)).await?;
            }
            let expected = observations("graphite-payload");
            assert_eq!(sender.0.lock().unwrap().len(), expected.len());
            for (actual, expected) in sender.0.lock().unwrap().iter().zip(expected) {
                assert_eq!(actual, expected["body"].as_str().unwrap());
            }
            Ok(())
        })
    });
}
#[test]
fn graphite_http_keeps_error_body_empty_lines_and_cr() {
    run(|env| {
        Box::pin(async move {
            let (peer, seen) = replay("graphite-http").await?;
            let client = QueryableGraphiteClient::with_endpoint(&peer.url.clone(), env.context);
            assert_eq!(
                client
                    .query("a b+*", jcompat::jtime::parse_instant("2026-01-01T00:00:00Z").unwrap())
                    .await?,
                ["one\r", " ", "last"]
            );
            assert_eq!(*seen.lock().unwrap(), 1);
            peer.shutdown().await?;
            Ok(())
        })
    });
}
#[test]
fn production_graphite_queries_use_port_80_without_connecting() {
    run(|env| {
        Box::pin(async move {
            let client = env.clients().graphite_client("127.0.0.1");
            assert_eq!(client.port(), Some(80));
            Ok(())
        })
    });
}
#[test]
fn query_dtos_round_trip_the_jvm_exchanges() {
    run(|env| {
        Box::pin(async move {
            let (peer, seen) = replay("v1-query").await?;
            let client = InfluxClient::connect(&v1(&peer.url), false, env.context.clone(), env.io.clone()).await?;
            let result = client.query("SELECT x FROM tests", "db").await?;
            assert_eq!(
                marathon_core::analytics::remote::influx::map_result(result, "percentile")?,
                [("pkg.Clazz#method".into(), 5000.0)]
            );
            client.shutdown().await?;
            assert_eq!(*seen.lock().unwrap(), 2);
            peer.shutdown().await?;
            let (peer, seen) = replay("v2-query").await?;
            let client = Influx2Client::connect(&v2(&peer.url), false, env.context.clone()).await?;
            let result = client.query("from(bucket: \"bucket\")").await?;
            assert_eq!(
                marathon_core::analytics::remote::influx2::map_records(result)?,
                [("pkg.Clazz#method".into(), 5000.0)]
            );
            client.shutdown().await?;
            assert_eq!(*seen.lock().unwrap(), 3);
            peer.shutdown().await?;
            Ok(())
        })
    });
}
#[test]
fn annotated_csv_tables_quotes_defaults_and_nulls() {
    let rows=marathon_analytics::influx2::decode("#datatype,string,long,string,double\n#group,false,false,true,false\n#default,_result,,fallback,\n,result,table,testname,_value\n,,0,\"a,b\",2.5\n,,0,,\n\n#datatype,string,long,string,double\n#default,_result,,,\n,result,table,testname,_value\n,,1,\"a\"\"b\",+Inf\n").unwrap();
    use marathon_core::analytics::remote::influx::Value;
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][&"testname".to_owned()], Value::Text("a,b".into()));
    assert_eq!(rows[1][&"testname".to_owned()], Value::Text("fallback".into()));
    assert_eq!(rows[1][&"_value".to_owned()], Value::Null);
    assert_eq!(rows[2][&"testname".to_owned()], Value::Text("a\"b".into()));
    assert_eq!(rows[2][&"_value".to_owned()], Value::Number(f64::INFINITY));
    assert!(marathon_analytics::influx2::decode("invalid").is_err());
    assert!(marathon_analytics::influx2::decode(",error,reference\n,bad query,7\n").is_err());
}
#[test]
fn v2_tracker_leaves_http_and_transport_to_the_guard_and_preserves_cancellation() {
    use marathon_core::report::Event;
    let logs = run(|env| {
        Box::pin(async move {
            let count = Arc::new(Mutex::new(0));
            let captured = count.clone();
            let peer = Peer::new(move |request| {
                if request.path.starts_with("/api/v2/orgs") {
                    Reply::ok("{\"orgs\":[{\"name\":\"org\",\"id\":\"id1\"}]}")
                } else if request.path.starts_with("/api/v2/buckets") {
                    Reply::ok("{\"buckets\":[{\"id\":\"bucket1\"}]}")
                } else {
                    *captured.lock().unwrap() += 1;
                    Reply {
                        status: 400,
                        body: "{\"code\":\"invalid\",\"message\":\"bad\"}".into(),
                        headers: vec![],
                    }
                }
            })
            .await?;
            let signal = Signal::new();
            let mut context = env.context.clone();
            context.cancellation = signal.clone();
            let client = Influx2Client::connect(&v2(&peer.url), false, context).await?;
            let tracker = Arc::new(Influx2Tracker::new(client.clone(), false)?);
            let class = |result: OperationResult<()>| match result {
                Err(OperationError::Failed(f)) => f.kind.description().to_owned(),
                other => panic!("{other:?}"),
            };
            assert_eq!(
                class(tracker.track_test(&event(TestStatus::Passed, false)).await),
                "InfluxDB bad request"
            );
            assert_eq!(*count.lock().unwrap(), 1);
            // D51: Kotlin logs per test. The guard logs this once, however many tests fail this way.
            let context = env.context.clone();
            let guard = BackendGuard::new(
                tracker.clone(),
                Arc::new(move |_, level, message, cause| {
                    context.logs.push(marathon_core::logging::LogRecord {
                        target: module_path!().into(),
                        logger: "guard".into(),
                        at_millis: 0,
                        wall_millis: 0,
                        level,
                        message: message.to_string(),
                        cause: cause.cloned(),
                    })
                }),
                marathon_logging::LogSettings::default().component("guard"),
                BackendLimits::default(),
            );
            let tracked = Event::Test(event(TestStatus::Passed, false));
            guard.track(&tracked).await.unwrap();
            guard.track(&tracked).await.unwrap();
            assert_eq!(*count.lock().unwrap(), 3);
            peer.shutdown().await?;
            assert_eq!(
                class(tracker.track_test(&event(TestStatus::Failure, false)).await),
                "InfluxDB v2 failure"
            );
            signal.cancel(requested("stop"));
            assert!(matches!(
                tracker.track_test(&event(TestStatus::Passed, false)).await,
                Err(OperationError::Cancelled(_))
            ));
            client.shutdown().await?;
            Ok(())
        })
    });
    let errors = logs.iter().filter(|r| r.logger == "guard").collect::<Vec<_>>();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(
        errors[0].cause.as_ref().unwrap().kind.description().to_owned(),
        "InfluxDB bad request"
    );
}
#[test]
fn factory_falls_back_on_real_setup_failure() {
    let logs = run(|env| {
        Box::pin(async move {
            let peer = Peer::new(|_| Reply {
                status: 500,
                body: "{\"error\":\"bad\"}".into(),
                headers: vec![],
            })
            .await?;
            let clients = Arc::new(env.clients());
            let provider = MetricsProviderFactory::new(env.context.clone(), clients)?
                .create_for(&AnalyticsConfiguration::InfluxDb(v1(&peer.url)), false)
                .await?;
            assert!(provider.is_noop());
            peer.shutdown().await?;
            Ok(())
        })
    });
    assert!(
        logs.iter()
            .any(|r| r.message == "Cannot read history from the analytics backend")
    );
}
#[test]
fn p4_rejects_exotic_inputs_before_network_setup() {
    run(|env| {
        Box::pin(async move {
            for url in [
                "host",
                "ftp://host",
                "http:\\host",
                "http://host/\n",
                "http://user:pass@host",
                "http://host?q=1",
                "http://host#fragment",
            ] {
                assert!(
                    InfluxClient::connect(&v1(url), false, env.context.clone(), env.io.clone())
                        .await
                        .is_err()
                );
            }
            let mut config = v1("http://127.0.0.1:1");
            config.url = String::from_utf16_lossy(&[0xd800]);
            assert!(
                InfluxClient::connect(&config, false, env.context, env.io)
                    .await
                    .is_err()
            );
            Ok(())
        })
    });
}
use marathon_config::AnalyticsConfiguration;

#[test]
fn real_transport_query_failures_retry_three_times_and_remain_cached() {
    use marathon_core::wiring::MetricsProvider;
    run(|env| {
        Box::pin(async move {
            for backend in ["v1", "v2", "graphite"] {
                let count = Arc::new(Mutex::new(0));
                let queries = count.clone();
                let peer=Peer::new(move|request| {
                if request.body=="q=SHOW+DATABASES" {return Reply::ok("{\"results\":[{\"series\":[{\"name\":\"databases\",\"columns\":[\"name\"],\"values\":[[\"db\"]]}]}]}");}
                if request.path.starts_with("/api/v2/orgs") {return Reply::ok("{\"orgs\":[{\"id\":\"id1\",\"name\":\"org\"}]}");}
                if request.path.starts_with("/api/v2/buckets") {return Reply::ok("{\"buckets\":[{\"id\":\"bucket1\"}]}");}
                *queries.lock().unwrap()+=1;
                if backend=="v1" {Reply::ok("{\"results\":[{\"error\":\"statement failed\"}]}")}
                else if backend=="v2" {Reply::ok("invalid CSV")}
                else {Reply{status:500,body:"bad raw value".into(),headers:vec![]}}
            }).await?;
                let clients = Arc::new(env.clients().with_graphite_endpoint(peer.url.clone()));
                let config = match backend {
                    "v1" => AnalyticsConfiguration::InfluxDb(v1(&peer.url)),
                    "v2" => AnalyticsConfiguration::InfluxDb2(v2(&peer.url)),
                    _ => AnalyticsConfiguration::Graphite(marathon_config::GraphiteConfiguration {
                        host: "127.0.0.1".into(),
                        port: Some(2003),
                        prefix: None,
                        defaults: defaults(),
                        read_only: false,
                    }),
                };
                let provider = MetricsProviderFactory::new(env.context.clone(), clients)?
                    .create_for(&config, false)
                    .await?;
                let limit = jcompat::jtime::instant_of(0, 0).unwrap();
                assert_eq!(provider.execution_time(&test(), 90.0, limit).await?, 300000.0);
                assert_eq!(provider.execution_time(&test(), 90.0, limit).await?, 300000.0);
                assert_eq!(*count.lock().unwrap(), 3);
                provider.close().await?;
                peer.shutdown().await?;
            }
            Ok(())
        })
    });
}

fn bind_analytics(yaml: &str) -> AnalyticsConfiguration {
    use marathon_config::{
        bind::bind_analytics_configuration,
        binder::Binder,
        binding::{BindingContext, Profile},
        node,
    };
    use std::path::PathBuf;
    let context = BindingContext {
        profile: Profile::Marathonfile,
        marathonfile: PathBuf::from("/Marathonfile"),
        marathonfile_dir: PathBuf::from("/"),
        process_dir: PathBuf::from("/"),
        environment: vec![],
        now: jcompat::jtime::instant_of(0, 0).unwrap(),
        analytics_tracking: false,
        bugsnag_reporting: false,
    };
    let stream = node::load(yaml);
    let binder = Binder::new(&context, &stream);
    binder
        .first(&stream, bind_analytics_configuration)
        .unwrap()
        .flatten()
        .unwrap()
}
#[test]
fn yaml_read_only_bug_is_kept_while_direct_read_only_suppresses_writes() {
    run(|env| {
        Box::pin(async move {
            let peer=Peer::new(|request|if request.body=="q=SHOW+DATABASES"{Reply::ok("{\"results\":[{\"series\":[{\"name\":\"databases\",\"columns\":[\"name\"],\"values\":[[\"db\"]]}]}]}")}else{Reply{status:204,body:String::new(),headers:vec![]}}).await?;
            let config = bind_analytics(&format!(
                "type: influxdb\nurl: '{}'\nuser: ''\npassword: ''\ndbName: db\nreadOnly: true\n",
                peer.url
            ));
            let AnalyticsConfiguration::InfluxDb(config) = config else {
                panic!("wrong bound backend")
            };
            assert!(!config.read_only);
            let influx_client = InfluxClient::connect(&config, false, env.context.clone(), env.io).await?;
            InfluxTracker {
                client: influx_client.clone(),
                read_only: config.read_only,
            }
            .track_test(&event(TestStatus::Passed, false))
            .await?;
            InfluxTracker {
                client: influx_client.clone(),
                read_only: true,
            }
            .track_test(&event(TestStatus::Passed, false))
            .await?;
            influx_client.shutdown().await?;
            let requests = peer.shutdown().await?;
            assert_eq!(requests.iter().filter(|r| r.path.starts_with("/write")).count(), 1);
            let AnalyticsConfiguration::Graphite(config) =
                bind_analytics("type: graphite\nhost: 127.0.0.1\nreadOnly: true\n")
            else {
                panic!("wrong bound backend")
            };
            assert!(!config.read_only);
            let sender = Arc::new(Sender::default());
            let tracker = GraphiteTracker {
                client: client(sender.clone()),
                read_only: config.read_only,
            };
            tracker.track_test(&event(TestStatus::Passed, false)).await?;
            let tracker = GraphiteTracker {
                client: client(sender.clone()),
                read_only: true,
            };
            tracker.track_test(&event(TestStatus::Passed, false)).await?;
            assert_eq!(sender.0.lock().unwrap().len(), 1);
            Ok(())
        })
    });
}

#[test]
fn unicode_credentials_and_config_query_encoding_match_the_jvm() {
    run(|env| {
        Box::pin(async move {
            let (peer, seen) = replay("v1-config-escaping").await?;
            let mut config = v1(&peer.url);
            config.user = "u é😀".into();
            config.password = "päss".into();
            config.db_name = "db a+é".into();
            config.retention_policy_configuration.name = "rp a+é".into();
            let client = InfluxClient::connect(&config, false, env.context.clone(), env.io).await?;
            InfluxTracker {
                client: client.clone(),
                read_only: false,
            }
            .track_test(&event(TestStatus::Passed, false))
            .await?;
            client.query("SELECT x", &config.db_name).await?;
            client.shutdown().await?;
            assert_eq!(*seen.lock().unwrap(), 5);
            peer.shutdown().await?;
            let (peer, seen) = replay("v2-config-escaping").await?;
            let mut config = v2(&peer.url);
            config.bucket = "bucket a+é".into();
            let client = Influx2Client::connect(&config, false, env.context).await?;
            Influx2Tracker::new(client.clone(), false)?
                .track_test(&event(TestStatus::Passed, false))
                .await?;
            client.shutdown().await?;
            assert_eq!(*seen.lock().unwrap(), 4);
            peer.shutdown().await?;
            Ok(())
        })
    });
}

#[test]
fn http_follow_ups_match_okhttp_without_adding_history_retries() {
    run(|env| {
        Box::pin(async move {
            for scenario in ["v2-http408", "v2-http503", "v2-http503-retry", "v2-redirect"] {
                let (peer, seen) = replay(scenario).await?;
                let client = Influx2Client::connect(&v2(&peer.url), false, env.context.clone()).await?;
                if scenario == "v2-redirect" {
                    assert!(client.query("bad").await.is_err());
                } else {
                    // What counts here is the exchanges; a final 503 is `BackendGuard`'s to log (D51).
                    let _ = Influx2Tracker::new(client.clone(), false)?
                        .track_test(&event(TestStatus::Passed, false))
                        .await;
                }
                client.shutdown().await?;
                assert_eq!(*seen.lock().unwrap(), exchanges(scenario).len());
                peer.shutdown().await?;
            }
            Ok(())
        })
    });
}

#[test]
fn http_error_and_malformed_response_classes_match_the_jvm() {
    run(|env| {
        Box::pin(async move {
            for scenario in [
                "v2-error-400",
                "v2-error-401",
                "v2-error-429",
                "v2-error-500",
                "v2-invalid-csv",
            ] {
                let expected = exchanges(scenario)[0].clone();
                let observed = observations(scenario)[0].clone();
                let saved = expected.clone();
                let peer = Peer::new(move |request| {
                    if request.path.starts_with("/api/v2/orgs") {
                        return Reply::ok("{\"orgs\":[{\"id\":\"id1\",\"name\":\"org\"}]}");
                    }
                    if request.path.starts_with("/api/v2/buckets") {
                        return Reply::ok("{\"buckets\":[{\"id\":\"bucket1\"}]}");
                    }
                    assert_eq!(request.method, saved["method"]);
                    assert_eq!(request.path, saved["path"]);
                    assert_eq!(request.body, saved["body"]);
                    Reply {
                        status: saved["status"].as_u64().unwrap() as u16,
                        body: saved["response"].as_str().unwrap().into(),
                        headers: vec![],
                    }
                })
                .await?;
                let client = Influx2Client::connect(&v2(&peer.url), false, env.context.clone()).await?;
                let Err(OperationError::Failed(error)) = client.query("bad").await else {
                    panic!("invalid response accepted")
                };
                assert_eq!(
                    error.kind,
                    marathon_test_support::recorded_errors::kind(observed["class"].as_str().unwrap())
                );
                client.shutdown().await?;
                peer.shutdown().await?;
            }
            let (peer, _) = replay("v2-invalid-json").await?;
            let Err(OperationError::Failed(error)) = Influx2Client::connect(&v2(&peer.url), false, env.context).await
            else {
                panic!("invalid setup accepted")
            };
            assert_eq!(error.kind.description().to_owned(), "InfluxDB v2 failure");
            peer.shutdown().await?;
            Ok(())
        })
    });
}

#[test]
fn carbon_reset_errors_propagate_and_typed_cancellation_interrupts_send() {
    run(|_| {
        Box::pin(async {
            use tokio::net::TcpListener;
            let listener = io(TcpListener::bind("127.0.0.1:0").await)?;
            let port = io(listener.local_addr())?.port();
            let peer = tokio::spawn(async move {
                let (socket, _) = io(listener.accept().await)?;
                // A zero linger produces a deterministic reset without a blocking close.
                #[allow(deprecated)]
                let linger = socket.set_linger(Some(std::time::Duration::ZERO));
                io(linger)?;
                drop(socket);
                Ok::<_, OperationError>(())
            });
            let signal = Signal::new();
            let client = BasicGraphiteClient {
                host: "127.0.0.1".into(),
                port,
                prefix: None,
                sender: Arc::new(SocketSender::default()),
                cancellation: signal.clone(),
            };
            let result = client
                .send(GraphiteMetric {
                    metric_name: "m".repeat(8 * 1024 * 1024),
                    metric_value: "1".into(),
                    timestamp: 0,
                })
                .await;
            peer.await.map_err(|e| {
                marathon_core::executor::shared::failure(
                    FailureKind::Injected(InjectedKind::TestCarbon),
                    &e.to_string(),
                )
            })??;
            assert!(
                matches!(result,Err(OperationError::Failed(f)) if f.kind == marathon_error::FailureKind::Analytics(marathon_error::AnalyticsKind::Graphite) && f.message.as_ref().is_some_and(|m|m.to_owned().starts_with("Error while writing data to graphite: ")))
            );
            signal.cancel(requested("stop"));
            assert!(matches!(
                client
                    .send(GraphiteMetric {
                        metric_name: "metric".into(),
                        metric_value: "1".into(),
                        timestamp: 0
                    })
                    .await,
                Err(OperationError::Cancelled(_))
            ));
            Ok(())
        })
    });
}

#[test]
fn annotated_csv_resets_defaults_and_validates_integer_types() {
    use marathon_core::analytics::remote::influx::Value;
    let rows=marathon_analytics::influx2::decode("#datatype,string,long,string,double\n#default,_result,,old,\n,result,table,testname,_value\n,,0,,1.0\n#group,false,false,true,false\n#datatype,string,long,string,double\n,result,table,testname,_value\n,,1,,2.0\n").unwrap();
    assert_eq!(rows[0][&"testname".to_owned()], Value::Text("old".into()));
    assert_eq!(rows[1][&"testname".to_owned()], Value::Null);
    assert!(
        marathon_analytics::influx2::decode(
            "#datatype,string,long,string,long\n,result,table,testname,_value\n,,0,test,1.5\n"
        )
        .is_err()
    );
}

#[test]
fn carbon_cancellation_closes_a_pending_socket_write() {
    run(|_| {
        Box::pin(async {
            use tokio::{io::AsyncReadExt, net::TcpListener};
            let listener = io(TcpListener::bind("127.0.0.1:0").await)?;
            let port = io(listener.local_addr())?.port();
            let signal = Signal::new();
            let client = BasicGraphiteClient {
                host: "127.0.0.1".into(),
                port,
                prefix: None,
                sender: Arc::new(SocketSender::default()),
                cancellation: signal.clone(),
            };
            let send = client.send(GraphiteMetric {
                metric_name: "m".repeat(16 * 1024 * 1024),
                metric_value: "1".into(),
                timestamp: 0,
            });
            tokio::pin!(send);
            let (mut peer, _) = tokio::select! {accepted=listener.accept()=>io(accepted)?,result=&mut send=>panic!("send completed before peer read: {result:?}")};
            assert!(futures_util::poll!(&mut send).is_pending());
            signal.cancel(requested("interrupt pending write"));
            assert!(matches!(send.await, Err(OperationError::Cancelled(_))));
            let mut buffer = [0; 8192];
            let mut received = 0;
            loop {
                let count = io(peer.read(&mut buffer).await)?;
                if count == 0 {
                    break;
                }
                received += count;
            }
            assert!(received < 16 * 1024 * 1024);
            Ok(())
        })
    });
}

/// D51: a carbon that takes the data but never reads can't hold the queue up for long.
#[test]
fn a_stalled_carbon_write_times_out() {
    run(|_| {
        Box::pin(async {
            use tokio::net::TcpListener;
            let listener = io(TcpListener::bind("127.0.0.1:0").await)?;
            let port = io(listener.local_addr())?.port();
            let client = BasicGraphiteClient {
                host: "127.0.0.1".into(),
                port,
                prefix: None,
                sender: Arc::new(SocketSender {
                    limits: CarbonLimits {
                        connect: std::time::Duration::from_secs(60),
                        write: std::time::Duration::from_millis(100),
                    },
                }),
                cancellation: Signal::new(),
            };
            let send = client.send(GraphiteMetric {
                metric_name: "m".repeat(64 * 1024 * 1024),
                metric_value: "1".into(),
                timestamp: 0,
            });
            let (result, accepted) = tokio::join!(send, listener.accept());
            let _held_open_and_never_read = io(accepted)?;
            let Err(OperationError::Failed(failure)) = result else {
                panic!("{result:?}")
            };
            assert_eq!(
                failure.message.unwrap().to_owned(),
                "Error while writing data to graphite: write timed out after 100 ms"
            );
            assert!(failure.kind == marathon_error::FailureKind::Analytics(marathon_error::AnalyticsKind::Graphite));
            Ok(())
        })
    });
}

/// D51: a full accept queue drops the SYN, which is what a firewalled host looks like. A host that
/// doesn't saturate has nothing to show here; `saturated` tells us which of the two this was.
#[test]
fn a_carbon_that_never_accepts_times_out_on_connect() {
    run(|_| {
        Box::pin(async {
            let socket = io(tokio::net::TcpSocket::new_v4())?;
            io(socket.bind("127.0.0.1:0".parse().map_err(|_| {
                marathon_core::executor::shared::failure(FailureKind::Injected(InjectedKind::TestCarbon), "address")
            })?))?;
            let address = io(socket.local_addr())?;
            let listener = io(socket.listen(1))?;
            let mut held = Vec::new();
            let mut saturated = false;
            for _ in 0..512 {
                match tokio::time::timeout(
                    std::time::Duration::from_millis(250),
                    tokio::net::TcpStream::connect(address),
                )
                .await
                {
                    Ok(Ok(stream)) => held.push(stream),
                    Ok(Err(_)) | Err(_) => {
                        saturated = true;
                        break;
                    }
                }
            }
            if !saturated {
                eprintln!("the accept queue didn't fill on this host; the connect limit wasn't exercised");
                return Ok(());
            }
            let sender = SocketSender {
                limits: CarbonLimits {
                    connect: std::time::Duration::from_millis(100),
                    write: std::time::Duration::from_secs(60),
                },
            };
            // Unbounded, the OS's connect timeout takes over a minute, so this separates the two.
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(60),
                sender.send_data("127.0.0.1", address.port(), "m 1 0\n".into(), Signal::new()),
            )
            .await
            .map_err(|_| {
                marathon_core::executor::shared::failure(
                    FailureKind::Injected(InjectedKind::TestCarbon),
                    "the connect limit did not apply",
                )
            })?;
            drop((listener, held));
            if result.is_ok() {
                eprintln!("the accept queue admitted another connection; the connect limit wasn't exercised");
                return Ok(());
            }
            let Err(OperationError::Failed(failure)) = result else {
                panic!("{result:?}")
            };
            let message = failure.message.unwrap().to_owned();
            // A host may refuse instead of dropping the SYN; both end the send at once.
            assert!(
                message.starts_with("Error while writing data to graphite: "),
                "{message}"
            );
            Ok(())
        })
    });
}

/// D51 with the real tracker: carbon down is the realistic fault. Each test tries, none fails.
#[test]
fn a_refused_carbon_is_logged_once_through_the_real_tracker() {
    use marathon_core::report::Event;
    let logs = run(|env| {
        Box::pin(async move {
            // A privileged port nothing here listens on. A released ephemeral one could be taken by a parallel test.
            let port = 1;
            let tracker = Arc::new(GraphiteTracker {
                client: BasicGraphiteClient {
                    host: "127.0.0.1".into(),
                    port,
                    prefix: None,
                    sender: Arc::new(SocketSender::default()),
                    cancellation: Signal::new(),
                },
                read_only: false,
            });
            let context = env.context.clone();
            let guard = BackendGuard::new(
                tracker,
                Arc::new(move |_, level, message, cause| {
                    context.logs.push(marathon_core::logging::LogRecord {
                        target: module_path!().into(),
                        logger: "guard".into(),
                        at_millis: 0,
                        wall_millis: 0,
                        level,
                        message: message.to_string(),
                        cause: cause.cloned(),
                    })
                }),
                marathon_logging::LogSettings::default().component("guard"),
                BackendLimits {
                    slow_failure: std::time::Duration::from_secs(3600),
                    ..BackendLimits::default()
                },
            );
            let tracked = Event::Test(event(TestStatus::Passed, false));
            for _ in 0..5 {
                guard.track(&tracked).await.unwrap();
            }
            TrackerInternal::close(&guard).await?;
            Ok(())
        })
    });
    let lines = logs
        .iter()
        .filter(|r| r.logger == "guard")
        .map(|r| {
            (
                r.message.to_owned(),
                r.cause.as_ref().map(|c| c.kind.description().to_owned()),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[0].1.as_deref(), Some("Graphite failure"));
    assert_eq!(
        lines[1].0,
        "The analytics backend lost test metrics: 5 results failed to send, and up to 0 were skipped"
    );
}

#[test]
fn invalid_carbon_port_fails_at_the_write_boundary() {
    use marathon_analytics::BackendClients;
    use marathon_core::report::Event;
    run(|env| {
        Box::pin(async move {
            for read_only in [false, true] {
                let factory = BackendClients {
                    clients: Arc::new(env.clients()),
                    config: AnalyticsConfiguration::Graphite(marathon_config::GraphiteConfiguration {
                        host: "127.0.0.1".into(),
                        port: Some(-1),
                        prefix: None,
                        defaults: defaults(),
                        read_only,
                    }),
                    debug: false,
                };
                let tracker = factory
                    .create(BackendRequest::Graphite { port: Some(-1) })
                    .await?
                    .map_err(OperationError::Failed)?
                    .unwrap();
                tracker
                    .track(&Event::Test(event(TestStatus::Ignored, false)))
                    .await
                    .map_err(|e| OperationError::Failed(e.failure().clone()))?;
                let result = tracker.track(&Event::Test(event(TestStatus::Passed, false))).await;
                if read_only {
                    assert!(result.is_ok());
                } else {
                    assert!(
                        matches!(result,Err(marathon_core::error::VendorError::Other(f)) if f.kind == marathon_error::FailureKind::IllegalArgument)
                    );
                }
                tracker.close().await?;
            }
            Ok(())
        })
    });
}

#[test]
fn post_307_and_308_redirects_keep_method_and_body() {
    run(|env| {
        Box::pin(async move {
            for scenario in ["v2-write-307", "v2-write-308"] {
                let (peer, seen) = replay(scenario).await?;
                let client = Influx2Client::connect(&v2(&peer.url), false, env.context.clone()).await?;
                Influx2Tracker::new(client.clone(), false)?
                    .track_test(&event(TestStatus::Passed, false))
                    .await?;
                client.shutdown().await?;
                assert_eq!(*seen.lock().unwrap(), exchanges(scenario).len(), "{scenario}");
                peer.shutdown().await?;
            }
            let (peer, seen) = replay("v1-write-307").await?;
            let client = InfluxClient::connect(&v1(&peer.url), false, env.context.clone(), env.io.clone()).await?;
            InfluxTracker {
                client: client.clone(),
                read_only: false,
            }
            .track_test(&event(TestStatus::Passed, false))
            .await?;
            client.flush().await?;
            client.shutdown().await?;
            assert_eq!(*seen.lock().unwrap(), exchanges("v1-write-307").len());
            peer.shutdown().await?;
            Ok(())
        })
    });
}

#[test]
fn v2_empty_token_still_sends_the_token_scheme() {
    run(|env| {
        Box::pin(async move {
            assert_eq!(observations("v2-empty-token")[0]["result"], "ok");
            let (peer, seen) = replay("v2-empty-token").await?;
            let mut config = v2(&peer.url);
            config.token = "".into();
            let client = Influx2Client::connect(&config, false, env.context.clone()).await?;
            Influx2Tracker::new(client.clone(), false)?
                .track_test(&event(TestStatus::Passed, false))
                .await?;
            client.shutdown().await?;
            assert_eq!(*seen.lock().unwrap(), exchanges("v2-empty-token").len());
            peer.shutdown().await?;
            Ok(())
        })
    });
}

/// Serves setup for an existing database/bucket, then the recorded query exchange.
async fn query_peer(scenario: &str) -> OperationResult<Peer> {
    let saved = exchanges(scenario)[0].clone();
    Peer::new(move |request| {
        if request.body == "q=SHOW+DATABASES" {
            return Reply::ok(
                "{\"results\":[{\"series\":[{\"name\":\"databases\",\"columns\":[\"name\"],\"values\":[[\"db\"]]}]}]}",
            );
        }
        if request.path.starts_with("/api/v2/orgs") {
            return Reply::ok("{\"orgs\":[{\"id\":\"id1\",\"name\":\"org\"}]}");
        }
        if request.path.starts_with("/api/v2/buckets") {
            return Reply::ok("{\"buckets\":[{\"id\":\"bucket1\"}]}");
        }
        assert_eq!(request.method, saved["method"]);
        assert_eq!(request.path, saved["path"]);
        assert_eq!(request.body, saved["body"]);
        Reply {
            status: saved["status"].as_u64().unwrap() as u16,
            body: saved["response"].as_str().unwrap().into(),
            headers: vec![],
        }
    })
    .await
}

#[test]
fn v1_query_errors_keep_influxdb_exception_subclasses() {
    run(|env| {
        Box::pin(async move {
            for scenario in ["v1-error-database", "v1-error-field", "v1-error-auth", "v1-error-other"] {
                let peer = query_peer(scenario).await?;
                let client = InfluxClient::connect(&v1(&peer.url), false, env.context.clone(), env.io.clone()).await?;
                let Err(OperationError::Failed(error)) = client.query("SELECT x FROM tests", "db").await else {
                    panic!("{scenario}: error accepted")
                };
                let expected = &observations(scenario)[0];
                assert_eq!(
                    error.kind.dedup_key(),
                    marathon_test_support::recorded_errors::kind(expected["class"].as_str().unwrap()).dedup_key(),
                    "{scenario}"
                );
                assert_eq!(
                    error.message.map(|m| m.to_owned()).as_deref(),
                    expected["message"].as_str()
                );
                assert!(error.cause.is_none());
                client.shutdown().await?;
                peer.shutdown().await?;
            }
            Ok(())
        })
    });
}

#[test]
fn v2_invalid_numeric_cells_raise_influx_exception_caused_by_number_format() {
    run(|env| {
        Box::pin(async move {
            for scenario in ["v2-csv-long", "v2-csv-double"] {
                let peer = query_peer(scenario).await?;
                let client = Influx2Client::connect(&v2(&peer.url), false, env.context.clone()).await?;
                let Err(OperationError::Failed(error)) = client.query("bad").await else {
                    panic!("{scenario}: invalid cell accepted")
                };
                let expected = &observations(scenario)[0];
                assert_eq!(
                    error.kind.dedup_key(),
                    marathon_test_support::recorded_errors::kind(expected["class"].as_str().unwrap()).dedup_key(),
                    "{scenario}"
                );
                assert_eq!(
                    error.message.map(|m| m.to_owned()).as_deref(),
                    expected["message"].as_str()
                );
                assert_eq!(
                    error.cause.map(|c| c.kind),
                    expected["cause"]
                        .as_str()
                        .map(marathon_test_support::recorded_errors::kind)
                );
                client.shutdown().await?;
                peer.shutdown().await?;
            }
            Ok(())
        })
    });
}

type Change = fn(&mut marathon_config::InfluxDbConfiguration);
#[test]
fn setup_validation_messages_and_order_match_the_jars() {
    run(|env| {
        Box::pin(async move {
            let peer = Peer::new(|request| {
                if request.body == "q=SHOW+DATABASES" {
                    return Reply::ok("{\"results\":[{\"series\":[{\"name\":\"databases\",\"columns\":[\"name\"],\"values\":[[\"other\"]]}]}]}");
                }
                if request.path.starts_with("/api/v2/orgs") {
                    return Reply::ok("{\"orgs\":[{\"id\":\"id1\",\"name\":\"org\"}]}");
                }
                Reply::ok("{\"results\":[{}]}")
            })
            .await?;
            let cases: [(&str, Change); 6] = [
                ("Expecting a non-empty string for name", |c| c.db_name = "".into()),
                ("Expecting a non-empty string for retentionPolicyName", |c| {
                    c.retention_policy_configuration.name = "".into()
                }),
                ("Expecting a non-empty string for retentionDuration", |c| {
                    c.retention_policy_configuration.duration = "".into();
                    c.retention_policy_configuration.replication_factor = 0;
                }),
                // Java checks the duration format before the replication factor.
                ("Invalid InfluxDB duration: 30x for retentionDuration", |c| {
                    c.retention_policy_configuration.duration = "30x".into();
                    c.retention_policy_configuration.replication_factor = 0;
                }),
                ("Invalid InfluxDB duration: 1y for shardDuration", |c| {
                    c.retention_policy_configuration.shard_duration = "1y".into();
                    c.retention_policy_configuration.replication_factor = 0;
                }),
                ("Expecting a positive number for replicationFactor", |c| {
                    c.retention_policy_configuration.replication_factor = 0
                }),
            ];
            for (message, change) in cases {
                let mut config = v1(&peer.url);
                change(&mut config);
                let Err(OperationError::Failed(error)) =
                    InfluxClient::connect(&config, false, env.context.clone(), env.io.clone()).await
                else {
                    panic!("{message}: invalid setup accepted")
                };
                assert_eq!(error.kind.description().to_owned(), "invalid argument");
                assert_eq!(error.message.map(|m| m.to_owned()).as_deref(), Some(message));
            }
            let mut config = v2(&peer.url);
            config.bucket = "".into();
            let Err(OperationError::Failed(error)) = Influx2Client::connect(&config, false, env.context.clone()).await
            else {
                panic!("empty bucket accepted")
            };
            assert_eq!(
                error.message.map(|m| m.to_owned()).as_deref(),
                Some("Expecting a non-empty string for Bucket Name")
            );
            peer.shutdown().await?;
            Ok(())
        })
    });
}

#[test]
fn invalid_graphite_hosts_fail_per_query_and_fall_back_to_defaults() {
    use marathon_core::wiring::MetricsProvider;
    run(|env| {
        Box::pin(async move {
            for host in ["graphite host", "::1"] {
                let client = env.clients().graphite_client(host);
                let limit = jcompat::jtime::instant_of(0, 0).unwrap();
                assert!(matches!(
                    client.query("target", limit).await,
                    Err(OperationError::Failed(f)) if f.kind == marathon_error::FailureKind::IllegalArgument
                ));
                let config = AnalyticsConfiguration::Graphite(marathon_config::GraphiteConfiguration {
                    host: host.into(),
                    port: Some(2003),
                    prefix: None,
                    defaults: defaults(),
                    read_only: false,
                });
                let provider = MetricsProviderFactory::new(env.context.clone(), Arc::new(env.clients()))?
                    .create_for(&config, false)
                    .await?;
                assert!(!provider.is_noop(), "{host}");
                assert_eq!(provider.execution_time(&test(), 90.0, limit).await?, 300000.0);
                assert_eq!(provider.success_rate(&test(), limit).await?, 0.0);
                provider.close().await?;
            }
            Ok(())
        })
    });
}

#[test]
fn refused_carbon_connection_raises_graphite_exception() {
    run(|env| {
        Box::pin(async move {
            let expected = &observations("carbon-refused")[0];
            let client = BasicGraphiteClient {
                host: "127.0.0.1".into(),
                port: 1,
                prefix: None,
                sender: Arc::new(SocketSender::default()),
                cancellation: env.context.cancellation.clone(),
            };
            let Err(OperationError::Failed(error)) = client
                .send(GraphiteMetric {
                    metric_name: "metric".into(),
                    metric_value: "1".into(),
                    timestamp: 0,
                })
                .await
            else {
                panic!("refused carbon send accepted")
            };
            assert_eq!(
                error.kind.dedup_key(),
                marathon_test_support::recorded_errors::kind(expected["class"].as_str().unwrap()).dedup_key()
            );
            // P2: the native socket wording follows the prefix.
            assert!(
                error
                    .message
                    .is_some_and(|m| { m.to_owned().starts_with("Error while writing data to graphite: ") })
            );
            Ok(())
        })
    });
}

/// Reads one HTTP request and returns its head; `None` at EOF.
async fn read_request(socket: &mut tokio::net::TcpStream) -> std::io::Result<Option<String>> {
    use tokio::io::AsyncReadExt;
    let (mut bytes, mut buffer) = (Vec::new(), [0; 8192]);
    loop {
        if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&bytes[..end]).into_owned();
            let length = head
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, v)| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                return Ok(Some(head));
            }
        }
        let count = socket.read(&mut buffer).await?;
        if count == 0 {
            return Ok(None);
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
}

/// S-x34-1: a server that answers InfluxDB's last flush a byte at a time never trips the clients'
/// idle timeout, so without D51's limit on the close the run's end waits for as long as it trickles.
/// The guard gives way at its limit, logs once, and the worker's socket is closed.
#[test]
fn a_last_flush_to_a_trickling_server_gives_way_at_the_close_limit() {
    use marathon_core::report::Event;
    use tokio::{io::AsyncWriteExt, net::TcpListener};
    let logs = run(|env| {
        Box::pin(async move {
            let listener = io(TcpListener::bind("127.0.0.1:0").await)?;
            let url = format!("http://{}", io(listener.local_addr())?);
            let (started, mut started_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
            let (ended, mut ended_rx) = tokio::sync::mpsc::unbounded_channel::<usize>();
            let server = tokio::spawn(async move {
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        return;
                    };
                    let (started, ended) = (started.clone(), ended.clone());
                    tokio::spawn(async move {
                        let Ok(Some(head)) = read_request(&mut socket).await else {
                            return;
                        };
                        if head.starts_with("POST /query") {
                            let body = "{\"results\":[{\"series\":[{\"name\":\"databases\",\"columns\":[\"name\"],\"values\":[[\"db\"]]}]}]}";
                            let reply = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            );
                            let _ = socket.write_all(reply.as_bytes()).await;
                            let _ = socket.shutdown().await;
                            return;
                        }
                        // A write: a body that never ends, a byte every 20 ms, until the client hangs up.
                        let _ = started.send(());
                        let mut chunks = 0;
                        if socket
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\n")
                            .await
                            .is_ok()
                        {
                            while socket.write_all(b"x").await.is_ok() {
                                chunks += 1;
                                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                            }
                        }
                        let _ = ended.send(chunks);
                    });
                }
            });
            let client = InfluxClient::connect(&v1(&url), false, env.context.clone(), env.io.clone()).await?;
            let tracker = Arc::new(InfluxTracker {
                client,
                read_only: false,
            });
            let context = env.context.clone();
            let guard = BackendGuard::new(
                tracker,
                Arc::new(move |_, level, message, cause| {
                    context.logs.push(marathon_core::logging::LogRecord {
                        target: module_path!().into(),
                        logger: "guard".into(),
                        at_millis: 0,
                        wall_millis: 0,
                        level,
                        message: message.to_string(),
                        cause: cause.cloned(),
                    })
                }),
                marathon_logging::LogSettings::default().component("guard"),
                BackendLimits {
                    send: std::time::Duration::from_millis(500),
                    ..BackendLimits::default()
                },
            );
            guard
                .track(&Event::Test(event(TestStatus::Passed, false)))
                .await
                .unwrap();
            // The worker's timed flush is under way and trickling; the close queues behind it.
            started_rx.recv().await.expect("the flush reaches the server");
            TrackerInternal::close(&guard).await?;
            // The worker's request is stopped: the server sees the client hang up.
            let chunks = ended_rx.recv().await.expect("the trickling exchange ends");
            assert!(chunks > 0);
            server.abort();
            Ok(())
        })
    });
    let lines = logs
        .iter()
        .filter(|r| r.logger == "guard")
        .map(|r| r.message.to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        lines,
        ["The analytics backend did not close in 500 ms; test metrics it still held are lost"]
    );
}
