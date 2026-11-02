//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/strategy/impl/sharding/CountShardingStrategy.kt
//! and impl/sharding/ParallelShardingStrategy.kt.

use crate::model::{Test, TestShard};
use marathon_config::schema::ShardingStrategyConfiguration;

#[derive(Clone, Debug)]
pub enum ShardingStrategy {
    Count { count: i32 },
    Parallel,
}

impl ShardingStrategy {
    pub fn from_config(cnf: &ShardingStrategyConfiguration) -> Self {
        match cnf {
            ShardingStrategyConfiguration::Count { count } => Self::Count { count: *count },
            ShardingStrategyConfiguration::Parallel => Self::Parallel,
        }
    }

    pub fn create_shard(&self, tests: &[Test]) -> TestShard {
        let tests = match self {
            // `(0 until count)` is empty for count <= 0; copies are consecutive.
            Self::Count { count } => {
                let copies = usize::try_from(*count).unwrap_or(0);
                tests
                    .iter()
                    .flat_map(|test| std::iter::repeat_n(test, copies))
                    .cloned()
                    .collect()
            }
            Self::Parallel => tests.to_vec(),
        };
        TestShard {
            tests,
            flaky_tests: Vec::new(),
        }
    }
}
