//! Port of core/src/test/kotlin/com/malinskiy/marathon/execution/strategy/impl/sharding/CountShardingStrategyTest.kt
//! and impl/sharding/ParallelShardingStrategyTest.kt.

mod strategy_support;

use marathon_config::schema::ShardingStrategyConfiguration as Cnf;
use marathon_core::strategy::ShardingStrategy;
use strategy_support::generate_tests;

#[test]
fn n_count_sharding_strategy_should_return_5_tests_for_list_of_1_test_and_n_5() {
    let strategy = ShardingStrategy::from_config(&Cnf::Count { count: 5 });
    let result = strategy.create_shard(&generate_tests(1));
    assert_eq!(result.tests.len(), 5);
    assert_eq!(result.flaky_tests.len(), 0);
}

#[test]
fn parallel_sharding_strategy_tests_should_save_list_of_tests_to_test_shard_without_modifications() {
    let strategy = ShardingStrategy::from_config(&Cnf::Parallel);
    let tests = generate_tests(100);
    let shard = strategy.create_shard(&tests);
    assert_eq!(shard.tests.len(), tests.len());
    assert_eq!(shard.tests, tests);
    assert_eq!(shard.flaky_tests.len(), 0);
}

#[test]
fn count_sharding_repeats_each_test_consecutively_and_drops_all_for_non_positive_counts() {
    let tests = generate_tests(2);
    let shard = ShardingStrategy::from_config(&Cnf::Count { count: 2 }).create_shard(&tests);
    assert_eq!(
        shard.tests,
        vec![tests[0].clone(), tests[0].clone(), tests[1].clone(), tests[1].clone()]
    );
    for count in [0, -1, i32::MIN] {
        let shard = ShardingStrategy::from_config(&Cnf::Count { count }).create_shard(&tests);
        assert!(shard.tests.is_empty() && shard.flaky_tests.is_empty(), "{count}");
    }
}
