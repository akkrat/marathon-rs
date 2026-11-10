//! Port of core/src/test/kotlin/com/malinskiy/marathon/analytics/metrics/remote/graphite/GraphiteDataSourceTest.kt
//! and analytics/metrics/remote/influx/InfluxDBDataSourceTest.kt.
mod metrics_support;
mod strategy_support;
use jcompat::jtime::{Instant, parse_instant};
use marathon_core::{
    analytics::remote::{graphite::*, influx::*, influx2::*, *},
    runtime::ServiceFuture,
};
use metrics_support::*;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
#[derive(Default)]
struct Graphite {
    lines: Vec<String>,
    targets: Mutex<Vec<String>>,
}
impl GraphiteQuery for Graphite {
    fn query<'a>(&'a self, target: &'a str, _: Instant) -> ServiceFuture<'a, Vec<String>> {
        Box::pin(async move {
            self.targets.lock().unwrap().push(target.into());
            Ok(self.lines.clone())
        })
    }
}
struct Influx {
    reply: QueryResult,
    commands: Mutex<Vec<(String, String)>>,
}
impl InfluxQuery for Influx {
    fn query<'a>(&'a self, c: &'a str, d: &'a str) -> ServiceFuture<'a, QueryResult> {
        Box::pin(async move {
            self.commands.lock().unwrap().push((c.to_string(), d.to_string()));
            Ok(self.reply.clone())
        })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
struct Flux;
impl FluxQuery for Flux {
    fn query<'a>(&'a self, _: &'a str) -> ServiceFuture<'a, Vec<Record>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn close(&self) -> ServiceFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
fn graphite(value: &str) -> GraphiteDataSource {
    GraphiteDataSource {
        client: Arc::new(Graphite {
            lines: vec![format!(
                "com--example--SingleTest--method,1601884200,1602488400,31536000|{value}"
            )],
            ..Graphite::default()
        }),
        prefix: None,
        context: None,
    }
}
fn limit() -> Instant {
    parse_instant("2019-08-24T08:25:09.219Z").unwrap()
}
mod graphite_tests {
    use super::*;
    #[test]
    fn should_return_actual_value_of_success_rate() {
        run(async {
            let rows = graphite("50").request_all_success_rates(limit()).await.unwrap();
            assert_eq!(
                rows,
                vec![SuccessRate {
                    test_name: "com.example.SingleTest.method".into(),
                    mean: 0.5
                }]
            );
        });
    }
    #[test]
    fn should_return_actual_value_of_execution_time() {
        run(async {
            let rows = graphite("5000.0")
                .request_all_execution_times(99.0, limit())
                .await
                .unwrap();
            assert_eq!(
                rows,
                vec![ExecutionTime {
                    test_name: "com.example.SingleTest.method".into(),
                    percentile: 5000.0
                }]
            );
        });
    }
}
async fn influx_case(time: bool, policy: &str) {
    let column = if time { "percentile" } else { "mean" };
    let value = if time { 5000.0 } else { 0.5 };
    let client = Arc::new(Influx {
        reply: QueryResult {
            error: None,
            results: vec![StatementResult {
                error: None,
                series: vec![Series {
                    name: "tests".into(),
                    tags: BTreeMap::from([("testname".into(), "com.example.SingleTest.method".into())]),
                    columns: vec!["time".into(), column.into()],
                    values: vec![vec![
                        Value::Text("2019-08-24T08:25:09.219Z".into()),
                        Value::Number(value),
                    ]],
                }],
            }],
        },
        commands: Mutex::default(),
    });
    let p = InfluxDBDataSource {
        client: client.clone(),
        database: "test".into(),
        retention_policy: policy.into(),
    };
    if time {
        assert_eq!(
            p.request_all_execution_times(99.0, limit()).await.unwrap(),
            vec![ExecutionTime {
                test_name: "com.example.SingleTest.method".into(),
                percentile: value
            }]
        );
    } else {
        assert_eq!(
            p.request_all_success_rates(limit()).await.unwrap(),
            vec![SuccessRate {
                test_name: "com.example.SingleTest.method".into(),
                mean: value
            }]
        );
    }
    let select = if time {
        "PERCENTILE(\"duration\",99.0)"
    } else {
        "MEAN(\"success\")"
    };
    assert_eq!(
        *client.commands.lock().unwrap(),
        vec![(
            format!(
                "SELECT {select}\nFROM \"{policy}\".\"tests\"\nWHERE time >= '2019-08-24T08:25:09.219Z'\nGROUP BY \"testname\""
            ),
            "test".into()
        )]
    );
}
mod influx_tests {
    use super::*;
    #[test]
    fn should_return_actual_value_of_success_rate() {
        run(influx_case(false, "rpMarathon"));
    }
    #[test]
    fn should_return_actual_value_of_execution_time() {
        run(influx_case(true, "rpMarathon"));
    }
}
#[test]
fn should_return_actual_value_of_success_rate_with_custom_retention_policy() {
    run(influx_case(false, "blah"));
}
#[test]
fn should_return_actual_value_of_execution_time_with_custom_retention_policy() {
    run(influx_case(true, "blah"));
}

#[test]
fn query_and_graphite_parser_jvm_rows() {
    for line in include_str!("../../../fixtures/oracle/metrics/queries.jsonl").lines() {
        let row: serde_json::Value = serde_json::from_str(line).unwrap();
        match row["backend"].as_str().unwrap() {
            "influx" => {
                let source = InfluxDBDataSource {
                    client: Arc::new(Influx {
                        reply: QueryResult::default(),
                        commands: Mutex::default(),
                    }),
                    database: "db".into(),
                    retention_policy: row["policy"].as_str().unwrap().into(),
                };
                let expected = row["command"].as_str().unwrap();
                let p = expected
                    .contains("PERCENTILE")
                    .then(|| jcompat::jnum::parse_double(row["p"].as_str().unwrap()).unwrap());
                assert_eq!(
                    source
                        .command(p, parse_instant(row["limit"].as_str().unwrap()).unwrap())
                        .to_owned(),
                    expected
                );
                assert_eq!(source.database.to_owned(), row["database"].as_str().unwrap());
            }
            "flux" => {
                let source = InfluxDB2DataSource {
                    client: Arc::new(Flux),
                    bucket: row["bucket"].as_str().unwrap().into(),
                };
                let expected = row["command"].as_str().unwrap();
                let p = expected
                    .contains("quantile")
                    .then(|| jcompat::jnum::parse_double(row["p"].as_str().unwrap()).unwrap());
                assert_eq!(
                    source
                        .command(p, parse_instant(row["limit"].as_str().unwrap()).unwrap())
                        .to_owned(),
                    expected
                );
            }
            "graphite" => {
                let source = GraphiteDataSource {
                    client: Arc::new(Graphite::default()),
                    prefix: row["prefix"].as_str().map(str::to_owned),
                    context: None,
                };
                let time = row["time"].as_bool().unwrap();
                assert_eq!(source.target(time.then_some(99.9)), row["target"].as_str().unwrap());
                // D51's allowance: the JVM throws on these rows, and `parse_line` stays that witness.
                // The data source skips exactly the lines whose whole value is `None`.
                let line = row["line"].as_str().unwrap();
                assert_eq!(has_no_datapoint(line), line.ends_with("|None"), "{line}");
                assert!(!has_no_datapoint(line) || !row["error"].is_null(), "{line}");
                match parse_line(line) {
                    Ok((name, value)) => {
                        let value = if time { value } else { value / 100.0 };
                        assert_eq!(name.to_owned(), row["name"].as_str().unwrap());
                        let bits = if value.is_nan() {
                            f64::NAN.to_bits()
                        } else {
                            value.to_bits()
                        };
                        assert_eq!(format!("{bits:016x}"), row["bits"].as_str().unwrap());
                        assert!(row["error"].is_null());
                    }
                    Err(marathon_core::error::OperationError::Failed(error)) => {
                        assert_eq!(
                            marathon_test_support::recorded_errors::kind(row["error"].as_str().unwrap()),
                            error.kind
                        );
                        assert_eq!(row["errorMessage"], error.message.unwrap().to_owned());
                    }
                    Err(error) => panic!("unexpected parser error: {error:?}"),
                }
            }
            _ => panic!("unknown row"),
        }
    }
}
#[test]
fn influx_mapping_drops_nulls_filters_series_and_rejects_errors() {
    let mut series = Series {
        name: "tests".into(),
        tags: BTreeMap::from([("testname".into(), "t".into())]),
        columns: vec!["mean".into()],
        values: vec![vec![Value::Null], vec![Value::Number(0.5)]],
    };
    let query = |s: Series| QueryResult {
        error: None,
        results: vec![StatementResult {
            error: None,
            series: vec![s],
        }],
    };
    assert_eq!(
        map_result(query(series.clone()), "mean").unwrap(),
        vec![("t".into(), 0.5)]
    );
    series.name = "other".into();
    assert!(map_result(query(series.clone()), "mean").unwrap().is_empty());
    series.name = "tests".into();
    series.tags.clear();
    assert!(map_result(query(series), "mean").unwrap().is_empty());
    assert!(
        map_result(
            QueryResult {
                error: Some("bad".into()),
                results: vec![]
            },
            "mean"
        )
        .is_err()
    );
    assert!(
        map_result(
            QueryResult {
                error: None,
                results: vec![StatementResult {
                    error: Some("bad".into()),
                    series: vec![]
                }]
            },
            "mean"
        )
        .is_err()
    );
    assert!(
        map_records(vec![BTreeMap::from([
            ("testname".into(), Value::Text("t".into())),
            ("_value".into(), Value::Null)
        ])])
        .unwrap()
        .is_empty()
    );
    assert!(
        map_records(vec![BTreeMap::from([
            ("testname".into(), Value::Text("t".into())),
            ("_value".into(), Value::Text("bad".into()))
        ])])
        .is_err()
    );
}

#[test]
fn one_malformed_graphite_datapoint_fails_the_whole_query_and_is_cached_for_the_run() {
    run(async {
        for bad in ["50,None", "none", "None ", "nope"] {
            let h = Harness::new();
            let client = Arc::new(Graphite {
                lines: vec![
                    "pkg0--clazz0--method0,1,2,3|50".into(),
                    format!("pkg1--clazz1--method1,1,2,3|{bad}"),
                ],
                ..Graphite::default()
            });
            let source = GraphiteDataSource {
                client: client.clone(),
                prefix: None,
                context: None,
            };
            let p = h.provider(Arc::new(source), 300000, None);
            let test = &strategy_support::generate_tests(1)[0];
            use marathon_core::wiring::MetricsProvider;
            for _ in 0..2 {
                assert_eq!(p.success_rate(test, limit()).await.unwrap(), 0.0);
                assert_eq!(p.execution_time(test, 80.0, limit()).await.unwrap(), 300000.0);
            }
            assert_eq!(client.targets.lock().unwrap().len(), 6);
            assert_eq!(
                h.logs
                    .take()
                    .iter()
                    .filter(|e| e.logger == "marathon_core::timing")
                    .count(),
                2
            );
        }
    });
}

/// D51 / C38: a series with no points in the window prints `None`, and its neighbours survive.
#[test]
fn a_none_datapoint_is_skipped_and_keeps_the_rest_of_the_query() {
    run(async {
        let source = GraphiteDataSource {
            client: Arc::new(Graphite {
                lines: vec![
                    "pkg0--clazz0--method0,1,2,3|50".into(),
                    "pkg1--clazz1--method1,1,2,3|None".into(),
                    "pkg2--clazz2--method2,1,2,3|None".into(),
                    "pkg3--clazz3--method3,1,2,3|25".into(),
                ],
                ..Graphite::default()
            }),
            prefix: None,
            context: None,
        };
        let rates = source.request_all_success_rates(limit()).await.unwrap();
        assert_eq!(
            rates,
            [
                SuccessRate {
                    test_name: "pkg0.clazz0.method0".into(),
                    mean: 0.5
                },
                SuccessRate {
                    test_name: "pkg3.clazz3.method3".into(),
                    mean: 0.25
                }
            ]
        );
        let times = source.request_all_execution_times(90.0, limit()).await.unwrap();
        assert_eq!(times.len(), 2);
        assert_eq!(times[1].percentile, 25.0);
    });
}

#[test]
fn a_skipped_none_datapoint_logs_at_debug_only() {
    run(async {
        let h = Harness::new();
        let source = GraphiteDataSource {
            client: Arc::new(Graphite {
                lines: vec!["pkg0--clazz0--method0,1,2,3|None".into()],
                ..Graphite::default()
            }),
            prefix: None,
            context: Some(h.context.clone()),
        };
        assert!(source.request_all_success_rates(limit()).await.unwrap().is_empty());
        let logs = h.logs.take();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].level, marathon_logging::Level::DEBUG);
        assert_eq!(
            logs[0].message.to_owned(),
            "Skipping a Graphite series with no datapoints: pkg0--clazz0--method0,1,2,3|None"
        );
    });
}

#[test]
fn influx_server_errors_match_jvm_logged_causes_after_retries() {
    use marathon_core::wiring::MetricsProvider;
    run(async {
        for line in include_str!("../../../fixtures/oracle/metrics/influx-errors.jsonl").lines() {
            let row: serde_json::Value = serde_json::from_str(line).unwrap();
            let error = Some(row["error"].as_str().unwrap().into());
            let reply = if row["statement"].as_bool().unwrap() {
                QueryResult {
                    error: None,
                    results: vec![StatementResult { error, series: vec![] }],
                }
            } else {
                QueryResult { error, results: vec![] }
            };
            let client = Arc::new(Influx {
                reply,
                commands: Mutex::default(),
            });
            let source = InfluxDBDataSource {
                client: client.clone(),
                database: "db".into(),
                retention_policy: "rp".into(),
            };
            let h = Harness::new();
            let provider = h.provider(Arc::new(source), 300000, None);
            let test = &strategy_support::generate_tests(1)[0];
            let value = if row["time"].as_bool().unwrap() {
                provider
                    .execution_time(test, 80.0, strategy_support::limit())
                    .await
                    .unwrap()
            } else {
                provider.success_rate(test, strategy_support::limit()).await.unwrap()
            };
            assert_eq!(format!("{:016x}", value.to_bits()), row["bits"]);
            assert_eq!(
                client.commands.lock().unwrap().len(),
                row["calls"].as_u64().unwrap() as usize
            );
            let events = h.logs.take();
            let causes: Vec<_> = events
                .iter()
                .filter_map(|e| {
                    e.cause.as_ref().map(|cause| {
                        serde_json::json!({"logger": e.logger, "level": e.level.as_str(),
                    "class": cause.kind.identifier(), "message": cause.message.as_ref().unwrap().to_owned()})
                    })
                })
                .collect();
            let mut expected_causes = row["causes"].clone();
            for cause in expected_causes.as_array_mut().unwrap() {
                marathon_test_support::recorded_errors::error_object(cause);
            }
            let expected_causes = marathon_test_support::recorded_logs::native_logs(&expected_causes);
            assert_eq!(causes, *expected_causes.as_array().unwrap());
            let logs: Vec<_> = events
                .iter()
                .map(|e| {
                    let mut message = e.message.to_owned();
                    if e.logger == "marathon_core::timing" {
                        message.truncate(message.rfind(" in ").unwrap());
                        message.push_str(" in <ms> ms");
                    }
                    serde_json::json!([e.logger, e.level.as_str(), message])
                })
                .collect();
            assert_eq!(
                logs,
                *marathon_test_support::recorded_logs::native_logs(&row["logs"])
                    .as_array()
                    .unwrap()
            );
        }
    });
}
