//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/external/ExecutionTimeCoverage.kt
use crate::{error::OperationResult, model::TestShard, wiring::MetricsProvider};
use marathon_config::{BatchingStrategyConfiguration, Configuration, SortingStrategyConfiguration};

pub async fn report_execution_time_coverage(
    config: &Configuration,
    shard: &TestShard,
    provider: &dyn MetricsProvider,
) -> OperationResult<()> {
    let tests: Vec<_> = shard.tests.iter().chain(&shard.flaky_tests).cloned().collect();
    if let BatchingStrategyConfiguration::FixedSize {
        duration_millis: Some(_),
        percentile: Some(percentile),
        time_limit: Some(limit),
        ..
    } = &config.batching_strategy
    {
        provider
            .report_execution_time_coverage(&tests, *percentile, *limit, "the batch duration limit")
            .await?;
    }
    if let SortingStrategyConfiguration::ExecutionTime { percentile, time_limit } = &config.sorting_strategy {
        provider
            .report_execution_time_coverage(&tests, *percentile, *time_limit, "execution time sorting")
            .await?;
    }
    Ok(())
}
