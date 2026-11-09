//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/queue/QueueActor.kt
//! queue operations; D60 replaces the reference heap with priority order and FIFO ties.

use std::collections::BTreeMap;

pub type KeyFunction<T, K> = Box<dyn FnMut(&T) -> K + Send>;

/// Stores each entry's insertion-time key; only a new insertion extracts a key.
pub struct OrderedQueue<T, K> {
    entries: BTreeMap<(K, u64), T>,
    next_sequence: u64,
    key: KeyFunction<T, K>,
}

impl<T, K: Ord> OrderedQueue<T, K> {
    pub fn new(key: KeyFunction<T, K>) -> Self {
        Self {
            entries: BTreeMap::new(),
            next_sequence: 0,
            key,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn peek(&self) -> Option<&T> {
        self.entries.first_key_value().map(|(_, item)| item)
    }
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.entries.values()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.next_sequence = 0;
    }

    pub fn add(&mut self, item: T) -> bool {
        self.offer(item)
    }

    pub fn offer(&mut self, item: T) -> bool {
        if self.next_sequence == u64::MAX {
            // Renumber stored keys, preserving FIFO within ties without any new lookups or draws.
            self.entries = std::mem::take(&mut self.entries)
                .into_iter()
                .enumerate()
                .map(|(sequence, ((key, _), item))| ((key, sequence as u64), item))
                .collect();
            self.next_sequence = self.entries.len() as u64;
            assert!(self.next_sequence < u64::MAX, "queue insertion sequence exhausted");
        }
        let key = (self.key)(&item);
        self.entries.insert((key, self.next_sequence), item);
        self.next_sequence += 1;
        true
    }

    pub fn add_all(&mut self, items: impl IntoIterator<Item = T>) -> bool {
        let mut changed = false;
        for item in items {
            self.offer(item);
            changed = true;
        }
        changed
    }

    pub fn poll(&mut self) -> Option<T> {
        self.entries.pop_first().map(|(_, item)| item)
    }

    /// Probe equality removes every matching copy, independently of its stored priority.
    pub fn remove_all(&mut self, probes: &[T]) -> bool
    where
        T: PartialEq,
    {
        let old_len = self.len();
        self.entries.retain(|_, item| !probes.contains(item));
        self.len() != old_len
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[derive(Clone, Debug)]
    struct Item {
        identity: u8,
        priority: i32,
        metadata: u64,
    }
    impl PartialEq for Item {
        fn eq(&self, other: &Self) -> bool {
            self.identity == other.identity
        }
    }

    fn contents<'a>(items: impl Iterator<Item = &'a Item>) -> Vec<(u8, i32, u64)> {
        items
            .map(|item| (item.identity, item.priority, item.metadata))
            .collect()
    }

    #[test]
    fn random_operations_match_a_stable_sort_reference_model() {
        for seed in 0..64u64 {
            let mut state = seed + 1;
            let mut next = || {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                state >> 32
            };
            let draws = Arc::new(AtomicUsize::new(0));
            let count = draws.clone();
            let mut queue = OrderedQueue::new(Box::new(move |item: &Item| {
                count.fetch_add(1, Ordering::Relaxed);
                item.priority
            }));
            let mut model: Vec<Item> = vec![];
            let mut inserted = 0;
            for step in 0..1000 {
                match next() % 5 {
                    0..=2 => {
                        let items: Vec<_> = (0..next() % 5)
                            .map(|_| Item {
                                identity: (next() % 12) as u8,
                                priority: (next() % 9) as i32 - 4,
                                metadata: next(),
                            })
                            .collect();
                        inserted += items.len();
                        assert_eq!(queue.add_all(items.clone()), !items.is_empty());
                        model.extend(items);
                        model.sort_by_key(|item| item.priority);
                    }
                    3 => {
                        let actual = queue.poll().into_iter().collect::<Vec<_>>();
                        let expected = if model.is_empty() {
                            vec![]
                        } else {
                            vec![model.remove(0)]
                        };
                        assert_eq!(
                            contents(actual.iter()),
                            contents(expected.iter()),
                            "seed {seed}, step {step}"
                        );
                    }
                    _ => {
                        let probe = Item {
                            identity: (next() % 12) as u8,
                            priority: 99,
                            metadata: next(),
                        };
                        let probes = if next() % 4 == 0 {
                            vec![]
                        } else {
                            vec![probe.clone(), probe]
                        };
                        let before = model.len();
                        model.retain(|item| !probes.contains(item));
                        assert_eq!(queue.remove_all(&probes), before != model.len());
                    }
                }
                assert_eq!(
                    contents(queue.iter()),
                    contents(model.iter()),
                    "seed {seed}, step {step}"
                );
                assert_eq!(contents(queue.peek().into_iter()), contents(model.first().into_iter()));
                assert_eq!(queue.len(), model.len());
                assert_eq!(queue.is_empty(), model.is_empty());
                assert_eq!(draws.load(Ordering::Relaxed), inserted);
            }
        }
    }

    #[test]
    fn sequence_compaction_keeps_stored_keys_and_fifo_ties() {
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let mut queue = OrderedQueue::new(Box::new(move |item: &(i32, char)| {
            count.fetch_add(1, Ordering::Relaxed);
            item.0
        }));
        queue.add_all([(2, 'a'), (1, 'b'), (2, 'c')]);
        queue.next_sequence = u64::MAX;
        queue.add((2, 'd'));
        assert_eq!(
            queue.iter().copied().collect::<Vec<_>>(),
            [(1, 'b'), (2, 'a'), (2, 'c'), (2, 'd')]
        );
        assert_eq!(calls.load(Ordering::Relaxed), 4);
        queue.clear();
        assert_eq!(queue.next_sequence, 0);
        assert_eq!(queue.poll(), None);
        assert!(queue.add_all([(0, 'e')]));
        assert_eq!(queue.peek(), Some(&(0, 'e')));
    }
}
