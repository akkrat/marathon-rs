//! Port of core/src/integrationTest/kotlin/com/malinskiy/marathon/analytics/metrics/remote/BaseMetricsProviderIntegrationTest.kt
//! and influx/InfluxDbProviderIntegrationTest.kt, influx2/InfluxDb2ProviderIntegrationTest.kt.
#[path = "support/cases.rs"]
mod cases;
mod support;
use jcompat::jtime::instant_of;
use marathon_analytics::{graphite::*, influx::*, influx2::*};
use marathon_core::{
    analytics::{
        remote::{
            RemoteDataSource,
            graphite::GraphiteDataSource,
            influx::{InfluxDBDataSource, InfluxQuery},
            influx2::{FluxQuery, InfluxDB2DataSource},
        },
        *,
    },
    error::*,
    executor::shared::failure,
    model::TestStatus,
    wiring::MetricsProvider,
};
use std::sync::Arc;
use support::*;
use tokio::{io::AsyncReadExt, net::TcpListener};

async fn case(env: Env, name: &str) -> OperationResult<()> {
    let (_, percentile, minutes, low, high) = cases::HISTORY_CASES.iter().copied().find(|c| c.0 == name).unwrap();
    let expected = low..=high;
    for backend in ["v1", "v2", "graphite"] {
        let scenario = format!("history-{backend}-{name}");
        let (peer, seen) = replay(&scenario).await?;
        let source: Arc<dyn RemoteDataSource> = match backend {
            "v1" => {
                let config = v1(&peer.url);
                let client = InfluxClient::connect(&config, false, env.context.clone(), env.io.clone()).await?;
                let tracker = InfluxTracker {
                    client: client.clone(),
                    read_only: false,
                };
                for event in events() {
                    tracker.track_test(&event).await?;
                }
                client.flush().await?;
                Arc::new(InfluxDBDataSource {
                    client,
                    database: config.db_name,
                    retention_policy: config.retention_policy_configuration.name,
                })
            }
            "v2" => {
                let config = v2(&peer.url);
                let client = Influx2Client::connect(&config, false, env.context.clone()).await?;
                let tracker = Influx2Tracker::new(client.clone(), false)?;
                for event in events() {
                    tracker.track_test(&event).await?;
                }
                Arc::new(InfluxDB2DataSource {
                    client,
                    bucket: config.bucket,
                })
            }
            _ => {
                let listener = io(TcpListener::bind("127.0.0.1:0").await)?;
                let port = io(listener.local_addr())?.port();
                let carbon_rows = exchanges(&scenario)
                    .into_iter()
                    .filter(|r| r["protocol"] == "carbon")
                    .collect::<Vec<_>>();
                let carbon = tokio::spawn(async move {
                    for row in carbon_rows {
                        let (mut stream, _) = io(listener.accept().await)?;
                        let mut data = String::new();
                        io(stream.read_to_string(&mut data).await)?;
                        assert_eq!(data, row["body"]);
                    }
                    Ok::<_, OperationError>(())
                });
                let tracker = GraphiteTracker {
                    client: BasicGraphiteClient {
                        host: "127.0.0.1".into(),
                        port,
                        prefix: Some("prefix".into()),
                        sender: Arc::new(SocketSender::default()),
                        cancellation: env.context.cancellation.clone(),
                    },
                    read_only: false,
                };
                for event in events() {
                    tracker.track_test(&event).await?;
                }
                carbon.await.map_err(|e| failure("test.Carbon", &e.to_string()))??;
                Arc::new(GraphiteDataSource {
                    client: Arc::new(QueryableGraphiteClient::with_endpoint(
                        &peer.url.clone(),
                        env.context.clone(),
                    )),
                    prefix: Some("prefix".into()),
                    context: None,
                })
            }
        };
        let provider = MetricsProviderImpl::new(source, defaults(), env.context.clone())?;
        let limit = instant_of(ANCHOR / 1000 - minutes * 60, 0).unwrap();
        let actual = match percentile {
            Some(p) => provider.execution_time(&test(), p, limit).await?,
            None => provider.success_rate(&test(), limit).await?,
        };
        assert!(expected.contains(&actual), "{scenario}: {actual}");
        assert_eq!(actual, observations(&scenario)[0]["value"].as_f64().unwrap());
        // A second lookup is served from the merged provider's cache.
        let cached = match percentile {
            Some(p) => provider.execution_time(&test(), p, limit).await?,
            None => provider.success_rate(&test(), limit).await?,
        };
        assert_eq!(cached, actual);
        provider.close().await?;
        assert_eq!(
            *seen.lock().unwrap(),
            exchanges(&scenario).iter().filter(|r| r["method"].is_string()).count()
        );
        peer.shutdown().await?;
    }
    Ok(())
}
macro_rules! history_case {
    ($name:ident,$case:literal,$p:expr,$minutes:expr,$range:expr) => {
        #[test]
        fn $name() {
            run(|env| Box::pin(case(env, $case)));
        }
    };
}
history_case!(on_empty_db_success_rate_default_value_is_0, "er", None, 0, 0.0..=0.0);
history_case!(
    execution_time_default_value_is_300_000,
    "et",
    Some(90.0),
    0,
    300000.0..=300000.0
);
history_case!(
    verify_execution_time_50_percentile_for_last_two_hours,
    "p50",
    Some(50.0),
    120,
    5000.0..=6000.0
);
history_case!(
    verify_execution_time_90_percentile_for_last_two_hours,
    "p90",
    Some(90.0),
    120,
    9000.0..=10000.0
);
history_case!(
    verify_execution_time_50_percentile_for_25_minutes,
    "r50",
    Some(50.0),
    25,
    2000.0..=2000.0
);
history_case!(
    verify_execution_time_90_percentile_for_25_minutes,
    "r90",
    Some(90.0),
    35,
    4000.0..=4000.0
);
history_case!(
    verify_test_success_rate_should_return_1_for_last_50_minutes,
    "s50",
    None,
    50,
    1.0..=1.0
);
history_case!(
    verify_test_success_rate_should_return_0_833_for_last_65_minutes,
    "s65",
    None,
    65,
    0.833..=0.834
);
history_case!(
    verify_test_success_rate_should_return_0_5_for_last_2_hours,
    "s120",
    None,
    120,
    0.5..=0.5
);

async fn recreation(env: Env, v2_backend: bool) -> OperationResult<()> {
    let state = Arc::new(std::sync::Mutex::new((false, 0usize, Vec::<String>::new())));
    let captured = state.clone();
    let peer=Peer::new(move|request|{
        let mut state=captured.lock().unwrap();
        if request.body=="q=SHOW+DATABASES" {return Reply::ok(format!("{{\"results\":[{{\"series\":[{{\"name\":\"databases\",\"columns\":[\"name\"],\"values\":[[\"{}\"]]}}]}}]}}",if state.0{"db"}else{"other"}));}
        if request.body.starts_with("q=CREATE+DATABASE"){state.0=true;state.1+=1;}
        if request.path.starts_with("/api/v2/orgs"){return Reply::ok("{\"orgs\":[{\"name\":\"org\",\"id\":\"id1\"}]}");}
        if request.path.starts_with("/api/v2/buckets") {
            if request.method=="POST"{state.0=true;state.1+=1;return Reply::ok("{\"id\":\"bucket1\"}");}
            return Reply::ok(if state.0{"{\"buckets\":[{\"name\":\"bucket\",\"id\":\"bucket1\"}]}"}else{"{\"buckets\":[]}"});
        }
        if request.path.starts_with("/write") || request.path.starts_with("/api/v2/write") {state.2.extend(request.body.lines().map(str::to_owned));return Reply{status:204,body:String::new(),headers:vec![]};}
        if request.path.starts_with("/query?") {assert_eq!(state.2.len(),10);return Reply::ok("{\"results\":[{\"series\":[{\"name\":\"tests\",\"tags\":{\"testname\":\"pkg.Clazz.method\"},\"columns\":[\"percentile\"],\"values\":[[5000.0]]}]}]}");}
        if request.path.starts_with("/api/v2/query") {assert_eq!(state.2.len(),20);return Reply::ok("#datatype,string,long,string,double\n#group,false,false,true,false\n#default,_result,,,\n,result,table,testname,_value\n,,0,pkg.Clazz.method,5000.0\n,,1,pkg.Clazz.method2,5000.0\n");}
        Reply::ok("{\"results\":[{}]}")
    }).await?;
    let source: Arc<dyn RemoteDataSource> = if v2_backend {
        let config = v2(&peer.url);
        Influx2Client::connect(&config, false, env.context.clone())
            .await?
            .shutdown()
            .await?;
        let second = Influx2Client::connect(&config, false, env.context.clone()).await?;
        let tracker = Influx2Tracker::new(second.clone(), false)?;
        for mut event in events() {
            tracker.track_test(&event).await?;
            Arc::make_mut(&mut event.test_result).test.method = "method2".into();
            tracker.track_test(&event).await?;
        }
        second.shutdown().await?;
        Arc::new(InfluxDB2DataSource {
            client: Influx2Client::connect(&config, false, env.context.clone()).await?,
            bucket: config.bucket,
        })
    } else {
        let config = v1(&peer.url);
        InfluxClient::connect(&config, false, env.context.clone(), env.io.clone())
            .await?
            .shutdown()
            .await?;
        let second = InfluxClient::connect(&config, false, env.context.clone(), env.io.clone()).await?;
        let tracker = InfluxTracker {
            client: second.clone(),
            read_only: false,
        };
        for event in events() {
            tracker.track_test(&event).await?;
        }
        second.shutdown().await?;
        Arc::new(InfluxDBDataSource {
            client: InfluxClient::connect(&config, false, env.context.clone(), env.io.clone()).await?,
            database: config.db_name,
            retention_policy: config.retention_policy_configuration.name,
        })
    };
    let provider = MetricsProviderImpl::new(source, defaults(), env.context)?;
    let limit = instant_of(ANCHOR / 1000 - 2 * 86400, 0).unwrap();
    assert_eq!(provider.execution_time(&test(), 50.0, limit).await?, 5000.0);
    if v2_backend {
        let mut test = test();
        test.method = "method2".into();
        assert_eq!(provider.execution_time(&test, 50.0, limit).await?, 5000.0);
    }
    provider.close().await?;
    assert_eq!(state.lock().unwrap().1, 1);
    peer.shutdown().await?;
    Ok(())
}
/// Byte-level replay of the JVM recreation probe: close the first client, track
/// and close through the second (v2 tracks a second test), query through a third.
async fn recreation_replay(env: Env, v2_backend: bool) -> OperationResult<()> {
    let scenario = if v2_backend {
        "recreate-v2-p50"
    } else {
        "recreate-v1-p50"
    };
    let (peer, seen) = replay(scenario).await?;
    if v2_backend {
        let config = v2(&peer.url);
        Influx2Client::connect(&config, false, env.context.clone())
            .await?
            .shutdown()
            .await?;
        let second = Influx2Client::connect(&config, false, env.context.clone()).await?;
        let tracker = Influx2Tracker::new(second.clone(), false)?;
        let mut event = event(TestStatus::Passed, false);
        tracker.track_test(&event).await?;
        Arc::make_mut(&mut event.test_result).test.method = "method2".into();
        tracker.track_test(&event).await?;
        second.shutdown().await?;
        let third = Influx2Client::connect(&config, false, env.context.clone()).await?;
        let records = third.query("from(bucket: \"bucket\")").await?;
        assert_eq!(
            marathon_core::analytics::remote::influx2::map_records(records)?,
            [("pkg.Clazz.method".into(), 5000.0)]
        );
        third.shutdown().await?;
    } else {
        let config = v1(&peer.url);
        InfluxClient::connect(&config, false, env.context.clone(), env.io.clone())
            .await?
            .shutdown()
            .await?;
        let second = InfluxClient::connect(&config, false, env.context.clone(), env.io.clone()).await?;
        InfluxTracker {
            client: second.clone(),
            read_only: false,
        }
        .track_test(&event(TestStatus::Passed, false))
        .await?;
        second.shutdown().await?;
        let third = InfluxClient::connect(&config, false, env.context.clone(), env.io.clone()).await?;
        let result = third.query("SELECT x FROM tests", "db").await?;
        assert_eq!(
            marathon_core::analytics::remote::influx::map_result(result, "percentile")?,
            [("pkg.Clazz.method".into(), 5000.0)]
        );
        third.shutdown().await?;
    }
    assert_eq!(*seen.lock().unwrap(), exchanges(scenario).len());
    peer.shutdown().await?;
    Ok(())
}
mod influx {
    use super::*;
    #[test]
    fn multiple_creations_should_still_have_the_same_configured_retention_policy() {
        run(|env| Box::pin(recreation(env, false)));
        run(|env| Box::pin(recreation_replay(env, false)));
    }
}
mod influx2 {
    use super::*;
    #[test]
    fn multiple_creations_should_still_have_the_same_configured_retention_policy() {
        run(|env| Box::pin(recreation(env, true)));
        run(|env| Box::pin(recreation_replay(env, true)));
    }
}
