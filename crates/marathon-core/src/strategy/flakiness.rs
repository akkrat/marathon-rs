//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/strategy/impl/flakiness/ProbabilityBasedFlakinessStrategy.kt
//! and impl/flakiness/IgnoreFlakinessStrategy.kt.

use super::{MetricsKey, MetricsLookup};
use crate::model::TestShard;
use jcompat::jtime::Instant;
use marathon_config::schema::FlakinessStrategyConfiguration;

#[derive(Clone, Debug)]
pub enum FlakinessStrategy {
    Ignore,
    ProbabilityBased {
        min_success_rate: f64,
        max_count: i32,
        time_limit: Instant,
    },
    /// D72, native only: `count` copies of every test, so each runs `count + 1` times.
    Fixed {
        count: i32,
    },
}

impl FlakinessStrategy {
    pub fn from_config(cnf: &FlakinessStrategyConfiguration) -> Self {
        match cnf {
            FlakinessStrategyConfiguration::Ignore => Self::Ignore,
            FlakinessStrategyConfiguration::Probability {
                min_success_rate,
                max_count,
                time_limit,
            } => Self::ProbabilityBased {
                min_success_rate: *min_success_rate,
                max_count: *max_count,
                time_limit: *time_limit,
            },
            FlakinessStrategyConfiguration::Fixed { count } => Self::Fixed { count: *count },
        }
    }

    /// The history queries `process` can make, for the prefetch.
    pub fn metrics_keys(&self) -> Vec<MetricsKey> {
        match self {
            Self::Ignore | Self::Fixed { .. } => Vec::new(),
            Self::ProbabilityBased { time_limit, .. } => vec![MetricsKey::SuccessRate { limit: *time_limit }],
        }
    }

    pub fn process(&self, shard: TestShard, metrics: &dyn MetricsLookup) -> TestShard {
        let (min_success_rate, max_count, time_limit) = match self {
            Self::Ignore => return shard,
            Self::Fixed { count } => {
                // Grouped per test in shard order, as probability's copies are.
                let copies = usize::try_from(*count).unwrap_or(0);
                let output = shard
                    .tests
                    .iter()
                    .flat_map(|test| std::iter::repeat_n(test.clone(), copies))
                    .collect();
                return TestShard {
                    flaky_tests: output,
                    ..shard
                };
            }
            Self::ProbabilityBased {
                min_success_rate,
                max_count,
                time_limit,
            } => (*min_success_rate, *max_count, *time_limit),
        };
        // D34: every test of the shard is judged by the same history.
        let view = metrics.view();
        let metrics = view.as_deref().unwrap_or(metrics);
        // D71: without history every test would look like it never passed, and get max_count copies.
        if !metrics.success_rate_available(time_limit, "flakinessStrategy probability adds no copies") {
            return TestShard {
                flaky_tests: Vec::new(),
                ..shard
            };
        }
        let mut output = Vec::new();
        for test in &shard.tests {
            let success_rate = metrics.success_rate(test, time_limit);
            if success_rate < min_success_rate {
                let max_fail_rate = 1.0 - min_success_rate;
                let mut current_fail_rate = 1.0 - success_rate;
                let mut counter = 0i32;
                while current_fail_rate > max_fail_rate && counter < max_count {
                    output.push(test.clone());
                    // Kotlin squares the running rate (C29) where its KDoc multiplies
                    // by the base rate: p, p², p⁴, ... instead of p, p², p³, ...
                    current_fail_rate *= current_fail_rate;
                    counter = counter.wrapping_add(1);
                }
            }
        }
        TestShard {
            flaky_tests: output,
            ..shard
        }
    }
}
