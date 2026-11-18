//! WP x87: use the production actor and compare each dispatch to the pre-index queue rules.

#[path = "support/queue.rs"]
mod support;

use indexmap::IndexMap;
use marathon_config::schema::{
    BatchingStrategyConfiguration, ExecutionMode, ExecutionStrategyConfiguration, SortingStrategyConfiguration,
};
use marathon_core::{
    execution::{FromQueue, queue::QueueMessage},
    model::{MetaProperty, Test, TestStatus},
};
use rand::{RngExt, SeedableRng, rngs::StdRng};
use std::{cmp::Reverse, collections::HashMap};
use support::*;

#[derive(Clone, Copy, Debug)]
enum Batching {
    ClassName,
    FixedSize { size: usize, last_mile: usize },
}

#[derive(Debug, Default)]
struct Coverage {
    excluded_retry: bool,
    used_own_retry: bool,
    fresh_and_own: bool,
    rejected_retry: bool,
    formatted_class_collision: bool,
    fixed_duplicate: bool,
    last_mile: bool,
}

struct ScanQueue {
    queued: Vec<Test>,
    priorities: HashMap<Test, u8>,
    retries: IndexMap<Test, Vec<usize>>,
}

impl ScanQueue {
    fn enqueue(&mut self, tests: impl IntoIterator<Item = Test>) {
        self.queued.extend(tests);
        self.queued.sort_by_key(|test| Reverse(self.priorities[test]));
    }

    fn batch(&mut self, batching: Batching, place: usize, seen: &mut Coverage) -> (Vec<Test>, Vec<(Test, usize)>) {
        let avoided: Vec<_> = self
            .retries
            .iter()
            .filter(|(test, places)| {
                let copies = self.queued.iter().filter(|queued| *queued == *test).count();
                copies > 0 && copies == places.iter().filter(|&&owner| owner == place).count()
            })
            .map(|(test, _)| test.clone())
            .collect();
        let parked: Vec<_> = self
            .queued
            .iter()
            .filter(|test| avoided.contains(test))
            .cloned()
            .collect();
        let restore = !parked.is_empty() && parked.len() != self.queued.len();
        if restore {
            seen.excluded_retry = true;
            self.queued.retain(|test| !avoided.contains(test));
        } else if !parked.is_empty() {
            seen.used_own_retry = true;
        }

        let batch = match batching {
            Batching::ClassName => {
                let head = &self.queued[0];
                let class = head.to_class_name('.');
                let group: Vec<_> = self
                    .queued
                    .iter()
                    .filter(|test| test.to_class_name('.') == class)
                    .cloned()
                    .collect();
                seen.formatted_class_collision |= group.iter().any(|test| test.pkg != head.pkg);
                self.queued.retain(|test| !group.contains(test));
                group
            }
            Batching::FixedSize { last_mile, .. } if self.queued.len() < last_mile => {
                seen.last_mile = true;
                vec![self.queued.remove(0)]
            }
            Batching::FixedSize { size, .. } => {
                let mut batch = Vec::new();
                let mut rejected = Vec::new();
                while batch.len() < size && rejected.len() < 2 * size && !self.queued.is_empty() {
                    let test = self.queued.remove(0);
                    if batch.contains(&test) {
                        seen.fixed_duplicate = true;
                        rejected.push(test);
                    } else {
                        batch.push(test);
                    }
                }
                self.enqueue(rejected);
                batch
            }
        };
        if restore {
            self.enqueue(parked);
        }

        seen.fresh_and_own |= batch.iter().any(|test| {
            let copies = self.queued.iter().chain(&batch).filter(|other| *other == test).count();
            self.retries
                .get(test)
                .is_some_and(|owners| owners.contains(&place) && owners.len() < copies)
        });

        let mut dispatched = Vec::new();
        for test in &batch {
            if let Some(owners) = self.retries.get_mut(test) {
                let remaining = self.queued.iter().filter(|other| *other == test).count();
                if remaining < owners.len() {
                    let index = owners.iter().position(|&owner| owner != place).unwrap_or(0);
                    dispatched.push((test.clone(), owners.remove(index)));
                    if owners.is_empty() {
                        self.retries.shift_remove(test);
                    }
                }
            }
        }
        (batch, dispatched)
    }
}

fn assert_tests(actual: &[Test], expected: &[Test], seed: u64, step: usize) {
    assert_eq!(actual, expected, "seed {seed}, step {step}");
    // Test equality ignores metadata. Equal copies must keep their own metadata and order.
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(
            actual.meta_properties[0].name, expected.meta_properties[0].name,
            "seed {seed}, step {step}"
        );
    }
}

#[test]
fn indexed_batches_match_scan_selection_with_retries_and_requeues_u213() {
    let mut config = configuration();
    config.sorting_strategy = SortingStrategyConfiguration::ExecutionTime {
        percentile: 90.0,
        time_limit: epoch(),
    };
    config.execution_strategy = ExecutionStrategyConfiguration {
        mode: ExecutionMode::AllSuccess,
        fast: false,
    };
    config.uncompleted_test_retry_quota = 256;

    run(move |core| {
        Box::pin(async move {
            let mut seen = Coverage::default();
            let devices = [device("device-a"), device("device-b")];
            let workers = [None, Some("worker-b")];
            for seed in 0..96 {
                let mut rng = StdRng::seed_from_u64(seed);
                let batching = if seed % 2 == 0 {
                    Batching::ClassName
                } else {
                    Batching::FixedSize {
                        size: rng.random_range(2..=6),
                        last_mile: rng.random_range(2..=7),
                    }
                };
                config.batching_strategy = match batching {
                    Batching::ClassName => BatchingStrategyConfiguration::ClassName,
                    Batching::FixedSize { size, last_mile } => BatchingStrategyConfiguration::FixedSize {
                        size: size as i32,
                        duration_millis: None,
                        percentile: None,
                        time_limit: None,
                        last_mile_length: last_mile as i32,
                    },
                };

                // Both the dotted and the blank-package names collide after formatting.
                let names = [
                    ("a.b", "C"),
                    ("a", "b.C"),
                    ("", "Bare"),
                    ("\u{a0}", "Bare"),
                    ("pkg", "Other"),
                ];
                let candidates: Vec<_> = names
                    .into_iter()
                    .enumerate()
                    // Progress also keys on the formatted method name. Only the class names collide here.
                    .flat_map(|(index, (pkg, class))| {
                        [1, 2, 3].map(|method| mtest(pkg, class, &format!("test-{index}-{method}")))
                    })
                    .collect();
                let draws: Vec<_> = (0..rng.random_range(15..=35))
                    .map(|copy| {
                        let mut test = candidates[rng.random_range(0..candidates.len())].clone();
                        test.meta_properties.push(MetaProperty {
                            name: format!("copy-{copy}"),
                            values: Default::default(),
                        });
                        test
                    })
                    .collect();
                let priorities: HashMap<Test, u8> = candidates
                    .iter()
                    .map(|test| (test.clone(), rng.random_range(1..=3)))
                    .collect();
                let mut setup = Setup::new(config.clone(), draws[..12].to_vec());
                setup.flaky = draws[12..].to_vec();
                setup.execution_times = priorities
                    .iter()
                    .map(|(test, &key)| (test.clone(), key.into()))
                    .collect();
                let actor = Queue::spawn(&core, setup);

                // D83 alternates equal-priority copies in rounds, irrespective of their shard source.
                let mut rounds: Vec<Vec<Test>> = vec![Vec::new()];
                for test in draws {
                    if let Some(round) = rounds.iter_mut().find(|round| !round.contains(&test)) {
                        round.push(test);
                    } else {
                        rounds.push(vec![test]);
                    }
                }
                let mut reference = ScanQueue {
                    queued: Vec::new(),
                    priorities,
                    retries: IndexMap::new(),
                };
                reference.enqueue(rounds.into_iter().flatten());
                for step in 0..128 {
                    if reference.queued.is_empty() {
                        break;
                    }
                    let place = rng.random_range(0..devices.len());
                    let device = &devices[place];
                    let (expected, dispatched) = reference.batch(batching, place, &mut seen);
                    let replies = actor.request(device, workers[place]).await;
                    let batch = replies
                        .iter()
                        .find_map(|reply| match reply {
                            FromQueue::ExecuteBatch(_, batch) => Some(batch.clone()),
                            _ => None,
                        })
                        .unwrap_or_else(|| panic!("seed {seed}, step {step}: no batch in {replies:?}"));
                    assert_tests(&batch.tests, &expected, seed, step);
                    assert_eq!(batch.id, format!("batch-{}", step + 1));
                    assert_tests(
                        &actor.snapshot.borrow().as_ref().unwrap().queue,
                        &reference.queued,
                        seed,
                        step,
                    );

                    if step < 48 && rng.random_range(0..5) == 0 {
                        seen.rejected_retry |= !dispatched.is_empty();
                        reference.enqueue(expected);
                        for (test, owner) in dispatched {
                            reference.retries.entry(test).or_default().push(owner);
                        }
                        assert!(actor.send(QueueMessage::RejectBatch(device.clone(), batch)).await);
                    } else {
                        let (retry, pass): (Vec<_>, Vec<_>) = expected
                            .into_iter()
                            .partition(|_| step < 48 && rng.random_range(0..4) == 0);
                        let results = |tests: &[Test], status| {
                            tests
                                .iter()
                                .map(|test| result(test, device, &batch.id, status, 0, 1))
                                .collect()
                        };
                        let outcome = batch_results(
                            results(&pass, TestStatus::Passed),
                            vec![],
                            results(&retry, TestStatus::Incomplete),
                        );
                        for test in &retry {
                            reference.retries.entry(test.clone()).or_default().push(place);
                        }
                        reference.enqueue(retry);
                        assert!(actor.send(QueueMessage::Completed(device.clone(), outcome)).await);
                    }
                    actor.replies();
                    let snapshot = actor.snapshot.borrow();
                    let snapshot = snapshot.as_ref().unwrap();
                    assert_tests(&snapshot.queue, &reference.queued, seed, step);
                    let by_place = |place: usize| -> Vec<_> {
                        reference
                            .retries
                            .iter()
                            .filter_map(|(test, owners)| {
                                let label = workers[place].unwrap_or(&devices[place].serial_number);
                                let matching = owners
                                    .iter()
                                    .filter(|&&owner| owner == place)
                                    .map(|_| label.to_owned())
                                    .collect::<Vec<_>>();
                                (!matching.is_empty()).then(|| (test.clone(), matching))
                            })
                            .collect()
                    };
                    assert_eq!(snapshot.retry_devices, by_place(0), "seed {seed}, step {step}");
                    assert_eq!(snapshot.retry_workers, by_place(1), "seed {seed}, step {step}");
                }
                assert!(reference.queued.is_empty(), "seed {seed}: queue did not drain");
            }
            assert!(
                seen.excluded_retry && seen.used_own_retry && seen.fresh_and_own && seen.rejected_retry,
                "{seen:?}"
            );
            assert!(
                seen.formatted_class_collision && seen.fixed_duplicate && seen.last_mile,
                "{seen:?}"
            );
            Ok(())
        })
    });
}
