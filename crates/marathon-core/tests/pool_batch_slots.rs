//! D50 through the real pool, device actor and queue: a run over two test APKs. No Kotlin counterpart; the Kotlin
//! rule (C33) is measured by reverting `strategy/batching.rs`, see docs/wp/x32-findings.md.
#[path = "support/pool.rs"]
mod support;
use jcompat::jtime::Instant;
use marathon_config::schema::SortingStrategyConfiguration;
use marathon_core::{
    error::VendorResult,
    model::*,
    strategy::MetricsLookup,
    vendor::{BundleRef, TestBundle, TestBundleIdentifier},
};
use std::{collections::HashMap, sync::Arc};
use support::*;

struct Apk(String);
impl TestBundle for Apk {
    fn id(&self) -> &String {
        &self.0
    }
}
/// A test's APK is its class name, as the Android vendor's map from test to test APK has it.
struct ByClass(HashMap<String, BundleRef>);
impl TestBundleIdentifier for ByClass {
    fn identify(&self, test: &Test) -> VendorResult<Option<BundleRef>> {
        Ok(self.0.get(&test.clazz.to_owned()).cloned())
    }
}
struct Durations(HashMap<Test, f64>);
impl MetricsLookup for Durations {
    fn success_rate(&self, _test: &Test, _limit: Instant) -> f64 {
        1.0
    }
    fn execution_time(&self, test: &Test, _percentile: f64, _limit: Instant) -> f64 {
        self.0.get(test).copied().unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug)]
enum Order {
    /// `NoSorting` over the list as parsed: all of one APK, then all of the other.
    Listed,
    /// `execution-time` with durations unrelated to the APK, which mixes the two.
    Sorted,
}

/// The batches a device runs for `count` tests split over two APKs.
fn batches(order: Order, count: usize, size: i32) -> Vec<Vec<String>> {
    let found = Arc::new(std::sync::Mutex::new(Vec::new()));
    let out = found.clone();
    scheduling_test(move |h| {
        Box::pin(async move {
            let tests: Vec<Test> = (0..count)
                .map(|i| mtest(if i < count / 2 { "ApkA" } else { "ApkB" }, &format!("m{i:04}")))
                .collect();
            let apks = ["ApkA", "ApkB"].map(|name| {
                let apk: BundleRef = BundleRef(Arc::new(Apk(name.into())));
                (name.to_owned(), apk)
            });
            // A fixed generator: the duration of a test has nothing to do with its APK.
            let mut state = 88_172_645_463_325_252u64;
            let durations = tests
                .iter()
                .map(|test| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    (test.clone(), (state % 100_000) as f64)
                })
                .collect();
            let device = TestDevice::plain("serial-A");
            let pool = h.start_pool(
                tests,
                PoolOptions {
                    pool: "test".into(),
                    configure: Box::new(move |c| {
                        c.batching_strategy = fixed_size(size);
                        if matches!(order, Order::Sorted) {
                            c.sorting_strategy = SortingStrategyConfiguration::ExecutionTime {
                                percentile: 90.0,
                                time_limit: jcompat::jtime::instant_of(0, 0).unwrap(),
                            };
                        }
                    }),
                    bundle_identifier: Some(Arc::new(ByClass(apks.into_iter().collect()))),
                    lookup: Some(Arc::new(Durations(durations))),
                    ..Default::default()
                },
            );
            pool.add(&device);
            h.await_pool_termination().await;
            assert!(pool.aggregate_result().await.unwrap());
            *out.lock().unwrap() = device.batches();
        })
    });
    found.lock().unwrap().clone()
}

/// The device records method names, and `batches` numbers the methods of the first APK first.
fn apk_of(method: &str, count: usize) -> bool {
    method[1..].parse::<usize>().unwrap() < count / 2
}

#[test]
fn two_apks_in_mixed_order_fill_every_batch_but_each_apks_last() {
    // At most one batch above the ideal count: the tail of one APK can straggle at the end.
    for (count, size, most) in [(200, 10, 21), (1000, 10, 101), (1000, 50, 21)] {
        let batches = batches(Order::Sorted, count, size);
        // Printed for the table in the findings: run this with --nocapture.
        eprintln!("sorted {count} tests size {size}: {} batches", batches.len());
        assert_eq!(batches.iter().map(Vec::len).sum::<usize>(), count);
        for batch in &batches {
            assert!(batch.len() <= size as usize);
            assert!(
                batch.iter().all(|t| apk_of(t, count) == apk_of(&batch[0], count)),
                "{batch:?}"
            );
        }
        assert!(batches.len() <= most, "{} batches", batches.len());
    }
}

#[test]
fn two_apks_in_listed_order_lose_at_most_the_batch_of_the_first_poll() {
    for (count, size, expected) in [(200, 10, 21), (1000, 10, 101)] {
        let batches = batches(Order::Listed, count, size);
        eprintln!("listed {count} tests size {size}: {} batches", batches.len());
        assert_eq!(batches.iter().map(Vec::len).sum::<usize>(), count);
        assert_eq!(batches.len(), expected);
    }
}
