//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/ArtifactProcessor.kt

use indexmap::IndexMap;

use marathon_core::executor::shared::SharedDeferred;
use std::{collections::VecDeque, sync::Arc};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AdmissionStats {
    pub admitted: i32,
    pub waited: i32,
    pub waited_for_run: i32,
    pub max_wait_millis: i64,
    pub p95_wait_millis: i64,
    pub total_wait_millis: i64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Admission {
    pub starts_worker: bool,
    pub depth: i32,
    pub worker_depth: i32,
    pub wait_millis: i64,
    pub blocked_by_run: Option<bool>,
}
pub struct Item<T> {
    pub id: u64,
    pub owner: String,
    pub worker: String,
    pub payload: T,
    pub enqueued_at: i64,
}
pub struct Waiter<T> {
    pub item: Item<T>,
    pub arrived_at: i64,
    pub blocked_by_run: Option<bool>,
    pub decided: Arc<SharedDeferred<Option<Admission>>>,
}
struct Pending {
    count: i32,
    done: Arc<SharedDeferred<()>>,
}
struct Worker<T> {
    held: i32,
    queue: VecDeque<Item<T>>,
}
pub type Decision = Arc<SharedDeferred<Option<Admission>>>;
pub type Admitted = (String, Decision, Admission);
#[derive(Default)]
pub struct Effects {
    pub admitted: Vec<Admitted>,
    pub barriers: Vec<Arc<SharedDeferred<()>>>,
}
/// Pure transitions: the shell performs spawns, notifications, callbacks and logs.
pub struct AdmissionCore<T> {
    pub capacity: i32,
    pub concurrency: i32,
    pub worker_capacity: i32,
    pub closed: bool,
    pub outstanding: i32,
    pub done: Arc<SharedDeferred<()>>,
    pending: IndexMap<String, Pending>,
    workers: IndexMap<String, Worker<T>>,
    pub(super) waiting: Vec<Waiter<T>>,
    stats: AdmissionStats,
    waits: Vec<i64>,
}
impl<T> AdmissionCore<T> {
    pub fn new(capacity: i32, concurrency: i32, worker_capacity: i32) -> Self {
        let done = SharedDeferred::new();
        done.complete(Ok(()));
        Self {
            capacity,
            concurrency,
            worker_capacity,
            closed: false,
            outstanding: 0,
            done,
            pending: IndexMap::new(),
            workers: IndexMap::new(),
            waiting: vec![],
            stats: AdmissionStats::default(),
            waits: vec![],
        }
    }
    pub fn admissible(&self, worker: &str) -> bool {
        self.outstanding < self.capacity && self.workers.get(worker).map_or(0, |w| w.held) < self.worker_capacity
    }
    pub fn admit(&mut self, mut waiter: Waiter<T>, now: i64) -> Admitted {
        let pending = self
            .pending
            .entry(waiter.item.owner.clone())
            .or_insert_with(|| Pending {
                count: 0,
                done: SharedDeferred::new(),
            });
        pending.count += 1;
        if self.outstanding == 0 {
            self.done = SharedDeferred::new();
        }
        self.outstanding += 1;
        waiter.item.enqueued_at = now;
        let worker = waiter.item.worker.clone();
        let starts_worker = !self.workers.contains_key(&worker);
        let entry = self.workers.entry(worker.clone()).or_insert_with(|| Worker {
            held: 0,
            queue: VecDeque::new(),
        });
        entry.held += 1;
        entry.queue.push_back(waiter.item);
        let wait_millis = if let Some(for_run) = waiter.blocked_by_run {
            let wait = now.wrapping_sub(waiter.arrived_at) / 1_000_000;
            self.waits.push(wait);
            self.stats.waited_for_run += i32::from(for_run);
            wait
        } else {
            0
        };
        self.stats.admitted += 1;
        (
            worker,
            waiter.decided,
            Admission {
                starts_worker,
                depth: self.outstanding,
                worker_depth: entry.held,
                wait_millis,
                blocked_by_run: waiter.blocked_by_run,
            },
        )
    }
    pub fn wait(&mut self, mut waiter: Waiter<T>) {
        waiter.blocked_by_run = Some(self.outstanding >= self.capacity);
        self.waiting.push(waiter);
    }
    pub fn cancel_waiter(&mut self, id: u64) -> Option<Item<T>> {
        let at = self.waiting.iter().position(|w| w.item.id == id)?;
        Some(self.waiting.remove(at).item)
    }
    pub fn take(&mut self, worker: &str) -> Option<Item<T>> {
        let entry = self.workers.get_mut(worker)?;
        if let Some(item) = entry.queue.pop_front() {
            return Some(item);
        }
        self.workers.shift_remove(worker);
        None
    }
    pub fn complete(&mut self, owner: &str, worker: &str, now: i64) -> Effects {
        self.complete_with_times(owner, worker, || now)
    }
    pub fn complete_with_times(&mut self, owner: &str, worker: &str, mut timestamp: impl FnMut() -> i64) -> Effects {
        let mut effects = Effects::default();
        if let Some(pending) = self.pending.get_mut(owner) {
            pending.count -= 1;
            if pending.count == 0 {
                effects.barriers.push(pending.done.clone());
                self.pending.shift_remove(owner);
            }
        }
        if let Some(entry) = self.workers.get_mut(worker) {
            entry.held -= 1;
        }
        self.outstanding -= 1;
        if self.outstanding == 0 {
            effects.barriers.push(self.done.clone());
        }
        if !self.closed {
            let mut at = 0;
            while self.outstanding < self.capacity && at < self.waiting.len() {
                if self.admissible(&self.waiting[at].item.worker) {
                    let waiter = self.waiting.remove(at);
                    effects.admitted.push(self.admit(waiter, timestamp()));
                } else {
                    at += 1;
                }
            }
        }
        effects
    }
    pub fn owner_done(&self, owner: &str) -> Option<Arc<SharedDeferred<()>>> {
        self.pending.get(owner).map(|p| p.done.clone())
    }
    pub fn close(&mut self) -> Vec<Waiter<T>> {
        self.closed = true;
        std::mem::take(&mut self.waiting)
    }
    pub fn residual(&mut self) -> Vec<Item<T>> {
        std::mem::take(&mut self.workers)
            .into_values()
            .flat_map(|w| w.queue)
            .collect()
    }
    pub fn pending(&self) -> Vec<(String, i32)> {
        self.pending.iter().map(|(o, p)| (o.clone(), p.count)).collect()
    }
    pub fn stats_snapshot(&self) -> (AdmissionStats, Vec<i64>) {
        (self.stats, self.waits.clone())
    }
    #[cfg(test)]
    pub fn stats(&self) -> AdmissionStats {
        Self::summarize(self.stats_snapshot())
    }
    pub fn summarize((recorded, mut waits): (AdmissionStats, Vec<i64>)) -> AdmissionStats {
        waits.sort();
        let admitted = recorded.admitted;
        let rank = admitted.wrapping_mul(95).wrapping_add(99) / 100;
        let immediate = admitted - waits.len() as i32;
        AdmissionStats {
            waited: waits.len() as i32,
            max_wait_millis: waits.last().copied().unwrap_or(0),
            p95_wait_millis: if rank <= immediate {
                0
            } else {
                waits[(rank - immediate - 1) as usize]
            },
            total_wait_millis: waits.iter().fold(0_i64, |a, b| a.wrapping_add(*b)),
            ..recorded
        }
    }
}
