//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/queue/QueueActor.kt
//! and its comparator-backed java.util.PriorityQueue (reference JDK 21).

use std::cmp::Ordering;

/// Java's comparator-backed min-heap, including its observable comparison order.
///
/// Equal priorities have no FIFO guarantee. Iteration follows the backing array,
/// as Java's iterator and `toArray` do. Equality for removal is independent of
/// the comparator, allowing equal tests with different priorities.
///
/// The comparator can retain state (for example lazy random or duration keys).
/// Elements cannot be mutably accessed while queued because that would silently
/// invalidate the heap. Java's null elements and exception paths are represented
/// by Rust's types; empty `peek` and `poll` return `None`.
pub struct JavaPriorityQueue<T, C> {
    heap: Vec<T>,
    comparator: C,
}

impl<T, C> JavaPriorityQueue<T, C>
where
    C: FnMut(&T, &T) -> Ordering,
{
    pub fn new(comparator: C) -> Self {
        Self {
            heap: Vec::new(),
            comparator,
        }
    }

    pub fn len(&self) -> usize {
        self.heap.len()
    }

    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    pub fn peek(&self) -> Option<&T> {
        self.heap.first()
    }

    /// Iterate in backing-array order, without invoking the comparator.
    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.heap.iter()
    }

    pub fn clear(&mut self) {
        self.heap.clear();
    }

    /// Insert one element, returning Java's always-true `offer` result.
    ///
    /// Kotlin's `queue.add(test)` (`QueueActor.kt:246`) maps here: `PriorityQueue.add` delegates to `offer`.
    pub fn offer(&mut self, item: T) -> bool {
        self.heap.push(item);
        self.sift_up(self.heap.len() - 1);
        true
    }

    /// Java's `addAll` offers each element in input order; it does not heapify.
    pub fn add_all(&mut self, items: impl IntoIterator<Item = T>) -> bool {
        let mut changed = false;
        for item in items {
            self.offer(item);
            changed = true;
        }
        changed
    }

    pub fn poll(&mut self) -> Option<T> {
        if self.heap.is_empty() {
            return None;
        }
        let result = self.heap.swap_remove(0);
        if !self.heap.is_empty() {
            self.sift_down(0);
        }
        Some(result)
    }

    fn sift_up(&mut self, mut index: usize) {
        while index > 0 {
            let parent = (index - 1) >> 1;
            if (self.comparator)(&self.heap[index], &self.heap[parent]) != Ordering::Less {
                break;
            }
            self.heap.swap(index, parent);
            index = parent;
        }
    }

    fn sift_down(&mut self, mut index: usize) -> usize {
        let half = self.heap.len() >> 1;
        while index < half {
            let mut child = (index << 1) + 1;
            let right = child + 1;
            // Java compares the siblings first and keeps the left one on ties.
            if right < self.heap.len() && (self.comparator)(&self.heap[child], &self.heap[right]) == Ordering::Greater {
                child = right;
            }
            if (self.comparator)(&self.heap[index], &self.heap[child]) != Ordering::Greater {
                break;
            }
            self.heap.swap(index, child);
            index = child;
        }
        index
    }
}

impl<T, C> JavaPriorityQueue<T, C>
where
    T: PartialEq,
    C: FnMut(&T, &T) -> Ordering,
{
    /// Remove the first equal element in backing-array order (`remove(Object)`).
    pub fn remove(&mut self, probe: &T) -> bool {
        let Some(index) = self.heap.iter().position(|item| probe == item) else {
            return false;
        };
        self.heap.swap_remove(index);
        if index < self.heap.len() && self.sift_down(index) == index {
            // removeAt tries upward repair only when downward repair did not move
            // the replacement. Even its unsuccessful comparisons are observable.
            self.sift_up(index);
        }
        true
    }

    /// Remove every element equal to a probe, using JDK 21's bulk-removal order.
    pub fn remove_all(&mut self, probes: &[T]) -> bool {
        let old_len = self.heap.len();
        self.heap.retain(|item| !probes.contains(item));
        if old_len == self.heap.len() {
            return false;
        }
        // bulkRemove stably compacts survivors and heapifies the whole array,
        // rather than repairing the heap after each individual removal.
        for index in (0..(self.heap.len() >> 1)).rev() {
            self.sift_down(index);
        }
        true
    }
}
