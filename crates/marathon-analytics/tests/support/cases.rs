//! Port of core/src/integrationTest/kotlin/com/malinskiy/marathon/analytics/metrics/remote/BaseMetricsProviderIntegrationTest.kt.
// name, percentile (None = success rate), lookback minutes, inclusive assertion bounds.
pub const HISTORY_CASES: [(&str, Option<f64>, i64, f64, f64); 9] = [
    ("er", None, 0, 0.0, 0.0),
    ("et", Some(90.0), 0, 300000.0, 300000.0),
    ("p50", Some(50.0), 120, 5000.0, 6000.0),
    ("p90", Some(90.0), 120, 9000.0, 10000.0),
    ("r50", Some(50.0), 25, 2000.0, 2000.0),
    ("r90", Some(90.0), 35, 4000.0, 4000.0),
    ("s50", None, 50, 1.0, 1.0),
    ("s65", None, 65, 0.833, 0.834),
    ("s120", None, 120, 0.5, 0.5),
];
