//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/queue/QueueActor.kt
//! queue operations; D60 replaces the reference heap with priority order and FIFO ties.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, hash_map::Entry},
    hash::Hash,
    sync::Arc,
};

pub type KeyFunction<T, K> = Box<dyn FnMut(&T) -> K + Send>;

type Position<K> = (K, u64);
type Positions<K> = BTreeSet<Position<K>>;

struct Copies<K> {
    positions: Positions<K>,
    class: Option<Arc<str>>,
}

struct ClassIndex<T, K> {
    name: fn(&T) -> String,
    positions: HashMap<Arc<str>, Positions<K>>,
}

/// Stores each entry's insertion-time key; only a new insertion extracts a key.
pub struct OrderedQueue<T, K> {
    entries: BTreeMap<Position<K>, Arc<T>>,
    copies: HashMap<Arc<T>, Copies<K>>,
    classes: Option<ClassIndex<T, K>>,
    next_sequence: u64,
    key: KeyFunction<T, K>,
}

impl<T: Clone + Eq + Hash, K: Clone + Ord> OrderedQueue<T, K> {
    pub fn new(key: KeyFunction<T, K>) -> Self {
        Self {
            entries: BTreeMap::new(),
            copies: HashMap::new(),
            classes: None,
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
        self.entries.first_key_value().map(|(_, item)| item.as_ref())
    }
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.entries.values().map(Arc::as_ref)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.copies.clear();
        if let Some(classes) = &mut self.classes {
            classes.positions.clear();
        }
        self.next_sequence = 0;
    }

    pub fn add(&mut self, item: T) -> bool {
        self.offer(item)
    }

    pub fn offer(&mut self, item: T) -> bool {
        if self.next_sequence == u64::MAX {
            // Renumber stored keys, preserving FIFO within ties without any new lookups or draws.
            for copies in self.copies.values_mut() {
                copies.positions.clear();
            }
            if let Some(classes) = &mut self.classes {
                classes.positions.clear();
            }
            for (sequence, ((key, _), item)) in std::mem::take(&mut self.entries).into_iter().enumerate() {
                self.insert((key, sequence as u64), item);
            }
            self.next_sequence = self.entries.len() as u64;
            assert!(self.next_sequence < u64::MAX, "queue insertion sequence exhausted");
        }
        let key = (self.key)(&item);
        self.insert((key, self.next_sequence), Arc::new(item));
        self.next_sequence += 1;
        true
    }

    fn insert(&mut self, position: Position<K>, item: Arc<T>) {
        let copies = self.copies.entry(item.clone()).or_insert_with(|| Copies {
            positions: BTreeSet::new(),
            class: None,
        });
        copies.positions.insert(position.clone());
        if let Some(classes) = &mut self.classes {
            let class = copies.class.get_or_insert_with(|| (classes.name)(&item).into());
            classes
                .positions
                .entry(class.clone())
                .or_default()
                .insert(position.clone());
        }
        self.entries.insert(position, item);
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
        let (position, item) = self.entries.pop_first()?;
        self.unindex(&position, &item);
        Some(Arc::unwrap_or_clone(item))
    }

    /// Probe equality removes every matching copy, independently of its stored priority.
    pub fn remove_all(&mut self, probes: &[T]) -> bool {
        let positions = self.positions_of(probes);
        let changed = !positions.is_empty();
        for position in positions {
            self.remove(&position);
        }
        changed
    }

    pub(crate) fn count(&self, probe: &T) -> usize {
        self.copies.get(probe).map_or(0, |copies| copies.positions.len())
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn indexed_copy_count(&self) -> usize {
        self.copies.values().map(|copies| copies.positions.len()).sum()
    }

    pub(crate) fn take_all(&mut self, probes: &[T]) -> Vec<T> {
        self.positions_of(probes)
            .iter()
            .map(|position| Arc::unwrap_or_clone(self.remove(position)))
            .collect()
    }

    fn positions_of(&self, probes: &[T]) -> Positions<K> {
        probes
            .iter()
            .filter_map(|probe| self.copies.get(probe))
            .flat_map(|copies| copies.positions.iter().cloned())
            .collect()
    }

    fn take_head_class_by(&mut self, name: fn(&T) -> String) -> Vec<T> {
        // The strategy is fixed for a production queue. Build the index on its first
        // class batch, then maintain it across polls, retries and removals.
        let classes = self.classes.get_or_insert_with(|| {
            let mut positions: HashMap<Arc<str>, Positions<K>> = HashMap::new();
            for (item, copies) in &mut self.copies {
                let class: Arc<str> = name(item).into();
                positions
                    .entry(class.clone())
                    .or_default()
                    .extend(copies.positions.iter().cloned());
                copies.class = Some(class);
            }
            ClassIndex { name, positions }
        });
        let Some((_, head)) = self.entries.first_key_value() else {
            return Vec::new();
        };
        let class = self.copies[head].class.as_ref().expect("class of queued test");
        let positions = classes.positions[class].clone();
        positions
            .iter()
            .map(|position| Arc::unwrap_or_clone(self.remove(position)))
            .collect()
    }

    fn remove(&mut self, position: &Position<K>) -> Arc<T> {
        let item = self.entries.remove(position).expect("indexed queue entry");
        self.unindex(position, &item);
        item
    }

    fn unindex(&mut self, position: &Position<K>, item: &Arc<T>) {
        let Entry::Occupied(mut identity) = self.copies.entry(item.clone()) else {
            unreachable!("queued test has a copy index");
        };
        let copies = identity.get_mut();
        copies.positions.remove(position);
        if let (Some(classes), Some(class)) = (&mut self.classes, &copies.class) {
            let Entry::Occupied(mut class) = classes.positions.entry(class.clone()) else {
                unreachable!("queued test has a class index");
            };
            class.get_mut().remove(position);
            if class.get().is_empty() {
                class.remove();
            }
        }
        if copies.positions.is_empty() {
            identity.remove();
        }
    }
}

impl<K: Clone + Ord> OrderedQueue<crate::model::Test, K> {
    pub(super) fn take_head_class(&mut self) -> Vec<crate::model::Test> {
        self.take_head_class_by(|test| test.to_class_name('.'))
    }
}

#[cfg(test)]
mod class_batch_timing;

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
    impl Eq for Item {}
    impl Hash for Item {
        fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
            self.identity.hash(state);
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

    #[test]
    fn class_index_tracks_compaction_clear_and_copy_removal() {
        // Groups cross priority levels, and some entries are equal copies.
        let class = |item: &(i32, char)| item.1.to_string();
        let mut queue = OrderedQueue::new(Box::new(|item: &(i32, char)| item.0));
        queue.add_all([(3, 'a'), (1, 'a'), (0, 'x'), (2, 'a'), (1, 'b'), (1, 'b')]);
        assert_eq!(queue.take_head_class_by(class), [(0, 'x')]);
        queue.next_sequence = u64::MAX;
        queue.add((1, 'a'));
        assert!(queue.remove_all(&[(2, 'a')]));
        assert_eq!(queue.poll(), Some((1, 'a')));
        assert_eq!(queue.take_head_class_by(class), [(1, 'b'), (1, 'b')]);
        assert_eq!(queue.take_head_class_by(class), [(1, 'a'), (3, 'a')]);
        queue.add((0, 'z'));
        queue.clear();
        queue.add((0, 'a'));
        assert_eq!(queue.take_head_class_by(class), [(0, 'a')]);
        assert!(queue.is_empty());
    }

    #[test]
    fn class_selection_formats_once_per_queued_test_u213() {
        static FORMATS: AtomicUsize = AtomicUsize::new(0);
        const COUNT: usize = 4096;
        let class = |item: &usize| {
            FORMATS.fetch_add(1, Ordering::Relaxed);
            item.to_string()
        };
        let mut queue = OrderedQueue::new(Box::new(|_: &usize| ()));
        queue.add_all(0..COUNT);
        queue.add(0);
        assert_eq!(queue.take_head_class_by(class), [0, 0]);
        assert_eq!(FORMATS.load(Ordering::Relaxed), COUNT);
        for item in 1..COUNT {
            assert_eq!(queue.take_head_class_by(class), [item]);
        }
        assert_eq!(FORMATS.load(Ordering::Relaxed), COUNT);
        assert!(queue.is_empty());
    }
}
