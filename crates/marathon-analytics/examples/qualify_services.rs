//! Port of core/src/integrationTest/kotlin/com/malinskiy/marathon/analytics/metrics/remote/BaseMetricsProviderIntegrationTest.kt
//! and influx/InfluxDbProviderIntegrationTest.kt, influx2/InfluxDb2ProviderIntegrationTest.kt.
//! Explicit Phase 7 qualification; never run by the default test gate.
#[path = "../tests/support/cases.rs"]
mod cases;
#[path = "../tests/support/mod.rs"]
mod support;
use jcompat::jtime::instant_of;
use marathon_analytics::{graphite::*, influx::*, influx2::*};
use marathon_core::{
    analytics::{
        remote::{
            RemoteDataSource, graphite::GraphiteDataSource, influx::InfluxDBDataSource, influx2::InfluxDB2DataSource,
        },
        *,
    },
    error::*,
    executor::{Signal, shared::failure},
    model::*,
    report::TestEvent,
    wiring::MetricsProvider,
};
use std::sync::Arc;
use support::*;
fn env_var(name: &str) -> String {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("missing {name}; use tools/oracle/analytics-client-probes/qualify-services"))
}
fn shifted(anchor: i64) -> Vec<TestEvent> {
    events()
        .into_iter()
        .map(|mut e| {
            let shift = anchor - ANCHOR;
            e.instant = instant_of(e.instant.seconds() + shift / 1000, 0).unwrap();
            let result = Arc::make_mut(&mut e.test_result);
            result.start_time += shift;
            result.end_time += shift;
            e
        })
        .collect()
}
async fn build(
    env: &Env,
    backend: &str,
    key: &str,
) -> OperationResult<(Arc<dyn RemoteDataSource>, Arc<dyn TrackerInternal>)> {
    Ok(match backend {
        "v1" => {
            let mut config = v1(&env_var("MRS_INFLUX1_URL"));
            config.db_name = key.into();
            let client = InfluxClient::connect(&config, false, env.context.clone(), env.io.clone()).await?;
            (
                Arc::new(InfluxDBDataSource {
                    client: client.clone(),
                    database: config.db_name,
                    retention_policy: config.retention_policy_configuration.name,
                }),
                Arc::new(InfluxTracker {
                    client,
                    read_only: false,
                }),
            )
        }
        "v2" => {
            let mut config = v2(&env_var("MRS_INFLUX2_URL"));
            config.bucket = key.into();
            let client = Influx2Client::connect(&config, false, env.context.clone()).await?;
            (
                Arc::new(InfluxDB2DataSource {
                    client: client.clone(),
                    bucket: config.bucket,
                }),
                Arc::new(Influx2Tracker::new(client, false)?),
            )
        }
        _ => {
            let port = env_var("MRS_GRAPHITE_PORT")
                .parse()
                .map_err(|_| failure("qualification.Config", "Invalid carbon port"))?;
            (
                Arc::new(GraphiteDataSource {
                    client: Arc::new(QueryableGraphiteClient::with_endpoint(
                        &env_var("MRS_GRAPHITE_HTTP_URL").into(),
                        env.context.clone(),
                    )),
                    prefix: Some(key.into()),
                    context: None,
                }),
                Arc::new(GraphiteTracker {
                    client: BasicGraphiteClient {
                        host: env_var("MRS_GRAPHITE_HOST"),
                        port,
                        prefix: Some(key.into()),
                        sender: Arc::new(SocketSender::default()),
                        cancellation: Signal::new(),
                    },
                    read_only: false,
                }),
            )
        }
    })
}
async fn insert(tracker: &dyn TrackerInternal, anchor: i64, two_tests: bool) -> OperationResult<()> {
    for mut event in shifted(anchor) {
        tracker
            .track(&marathon_core::report::Event::Test(event.clone()))
            .await
            .map_err(|e| OperationError::Failed(e.failure().clone()))?;
        if two_tests {
            Arc::make_mut(&mut event.test_result).test.method = "method2".into();
            tracker
                .track(&marathon_core::report::Event::Test(event))
                .await
                .map_err(|e| OperationError::Failed(e.failure().clone()))?;
        }
    }
    Ok(())
}
/// Number of stored duration points for the case's database, bucket or prefix.
async fn ingested(env: &Env, backend: &str, key: &str, anchor: i64) -> OperationResult<usize> {
    use marathon_core::analytics::remote::{graphite::GraphiteQuery, influx::*, influx2::FluxQuery};
    let number = |value: Option<&Value>| match value {
        Some(Value::Number(n)) => *n as usize,
        _ => 0,
    };
    Ok(match backend {
        "v1" => {
            let mut config = v1(&env_var("MRS_INFLUX1_URL"));
            config.db_name = key.into();
            let client = InfluxClient::connect(&config, false, env.context.clone(), env.io.clone()).await?;
            let result = client
                .query(
                    &"SELECT count(\"duration\") FROM \"rpMarathon\".\"tests\"".into(),
                    &key.into(),
                )
                .await;
            client.shutdown().await?;
            number(
                result?
                    .results
                    .first()
                    .and_then(|r| r.series.first())
                    .and_then(|s| s.values.first())
                    .and_then(|row| row.get(1)),
            )
        }
        "v2" => {
            let mut config = v2(&env_var("MRS_INFLUX2_URL"));
            config.bucket = key.into();
            let client = Influx2Client::connect(&config, false, env.context.clone()).await?;
            let flux = format!(
                "from(bucket: \"{key}\") |> range(start: 0) |> filter(fn: (r) => r._measurement == \"tests\" and r._field == \"duration\") |> group() |> count()"
            );
            let result = client.query(&flux.as_str().into()).await;
            client.shutdown().await?;
            number(result?.first().and_then(|r| r.get(&"_value".into())))
        }
        _ => {
            let client =
                QueryableGraphiteClient::with_endpoint(&env_var("MRS_GRAPHITE_HTTP_URL").into(), env.context.clone());
            let lines = client
                .query(
                    &format!("{key}.tests.*.*.*.*.*.duration"),
                    instant_of(anchor / 1000 - 86400, 0).unwrap(),
                )
                .await?;
            lines
                .iter()
                .filter_map(|line| line.split_once('|'))
                .flat_map(|(_, values)| values.split(','))
                .filter(|v| !v.is_empty() && *v != "None")
                .count()
        }
    })
}
async fn query(
    source: Arc<dyn RemoteDataSource>,
    env: &Env,
    percentile: Option<f64>,
    limit: jcompat::jtime::Instant,
    test: &Test,
) -> OperationResult<f64> {
    let provider = MetricsProviderImpl::new(source, defaults(), env.context.clone())?;
    match percentile {
        Some(p) => provider.execution_time(test, p, limit).await,
        None => provider.success_rate(test, limit).await,
    }
}
fn main() {
    let anchor = marathon_core::logging::host_millis() / 1000 * 1000;
    for backend in ["v1", "v2", "graphite"] {
        for (name, p, minutes, low, high) in cases::HISTORY_CASES {
            let key = format!("qualification_{}_{}_{}", std::process::id(), backend, name);
            run(move |env| {
                Box::pin(async move {
                    let (source, tracker) = build(&env, backend, &key).await?;
                    insert(tracker.as_ref(), anchor, false).await?;
                    // A v1 flush barrier is observable through tracker close; reopen history after it.
                    tracker.close().await?;
                    let (source, unused_tracker) = if backend != "graphite" {
                        build(&env, backend, &key).await?
                    } else {
                        (source, tracker)
                    };
                    let limit = instant_of(anchor / 1000 - minutes * 60, 0).unwrap();
                    // Readiness is the ingested point count, independent of the case under test;
                    // the case is then asserted exactly once.
                    while ingested(&env, backend, &key, anchor).await? < 10 {
                        env.context
                            .clock
                            .delay(250, Signal::new())
                            .await
                            .map_err(|e| OperationError::Failed(e.failure().clone()))?;
                    }
                    let value = query(source.clone(), &env, p, limit, &test()).await?;
                    assert!((low..=high).contains(&value), "{backend}-{name}: {value}");
                    source.close().await?;
                    unused_tracker.close().await?;
                    Ok(())
                })
            });
        }
    }
    for backend in ["v1", "v2"] {
        let key = format!("qualification_{}_{}_recreate", std::process::id(), backend);
        run(move |env| {
            Box::pin(async move {
                let (source, first) = build(&env, backend, &key).await?;
                first.close().await?;
                source.close().await?;
                let (_, second) = build(&env, backend, &key).await?;
                insert(second.as_ref(), anchor, backend == "v2").await?;
                second.close().await?;
                let (source, third) = build(&env, backend, &key).await?;
                let limit = instant_of(anchor / 1000 - 2 * 86400, 0).unwrap();
                assert_eq!(query(source.clone(), &env, Some(50.0), limit, &test()).await?, 5000.0);
                if backend == "v2" {
                    let mut test = test();
                    test.method = "method2".into();
                    assert_eq!(query(source.clone(), &env, Some(50.0), limit, &test).await?, 5000.0);
                }
                source.close().await?;
                third.close().await?;
                Ok(())
            })
        });
    }
    println!("27 history cases and 2 recreation cases passed against real services");
}
