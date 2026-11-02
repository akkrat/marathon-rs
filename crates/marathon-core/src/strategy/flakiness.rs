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
        }
    }

    /// The history queries `process` can make, for the prefetch.
    pub fn metrics_keys(&self) -> Vec<MetricsKey> {
        match self {
            Self::Ignore => Vec::new(),
            Self::ProbabilityBased { time_limit, .. } => vec![MetricsKey::SuccessRate { limit: *time_limit }],
        }
    }

    pub fn process(&self, shard: TestShard, metrics: &dyn MetricsLookup) -> TestShard {
        let Self::ProbabilityBased {
            min_success_rate,
            max_count,
            time_limit,
        } = self
        else {
            return shard;
        };
        let mut output = Vec::new();
        for test in &shard.tests {
            let success_rate = metrics.success_rate(test, *time_limit);
            if success_rate < *min_success_rate {
                let max_fail_rate = 1.0 - min_success_rate;
                let mut current_fail_rate = 1.0 - success_rate;
                let mut counter = 0i32;
                while current_fail_rate > max_fail_rate && counter < *max_count {
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
