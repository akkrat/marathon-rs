//! D103 counts failures only when another Android device in the same pool can finish tests.

use crate::support::lock;
use marathon_core::{execution::batch_health::ConsecutiveBatches, model::DevicePoolId};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

#[derive(Default)]
pub(crate) struct PeerProgress(Mutex<HashMap<DevicePoolId, Arc<PoolProgress>>>);

impl PeerProgress {
    pub(crate) fn for_pool(&self, pool: DevicePoolId) -> Arc<PoolProgress> {
        lock(&self.0).entry(pool).or_default().clone()
    }
}

/// A replacement on the same adb endpoint and serial is not another device.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PeerId(pub(crate) SocketAddr, pub(crate) Arc<str>);

#[derive(Default)]
struct Progress {
    generation: u64,
    latest: Option<PeerId>,
    preceding_peer: u64,
}

#[derive(Default)]
pub(crate) struct PoolProgress(Mutex<Progress>);

impl PoolProgress {
    pub(crate) fn snapshot(&self) -> u64 {
        lock(&self.0).generation
    }

    pub(crate) fn completed(&self, device: &PeerId) {
        let mut progress = lock(&self.0);
        if progress.latest.as_ref() != Some(device) {
            progress.preceding_peer = progress.generation;
            progress.latest = Some(device.clone());
        }
        progress.generation += 1;
    }

    fn peer_after(&self, device: &PeerId, since: u64) -> Option<u64> {
        let progress = lock(&self.0);
        // Only the newest completion by a different device matters. If the latest was ours,
        // use the generation just before our current run of consecutive completions.
        let peer = if progress.latest.as_ref() == Some(device) {
            progress.preceding_peer
        } else {
            progress.generation
        };
        (peer > since).then_some(progress.generation)
    }
}

pub(crate) struct BatchStreak {
    count: ConsecutiveBatches,
    cursor: Option<(Arc<PoolProgress>, u64)>,
}

impl Default for BatchStreak {
    fn default() -> Self {
        Self {
            count: ConsecutiveBatches::new(ConsecutiveBatches::DEFAULT_LIMIT),
            cursor: None,
        }
    }
}

impl BatchStreak {
    pub(crate) fn reset(&mut self) {
        self.count.reset();
        self.cursor = None;
    }

    pub(crate) fn strike(&mut self, pool: &Arc<PoolProgress>, device: &PeerId, batch_start: u64) -> Option<i32> {
        if self
            .cursor
            .as_ref()
            .is_some_and(|(previous, _)| !Arc::ptr_eq(previous, pool))
        {
            self.reset();
        }
        // The first eligible failure opens the streak at that batch's start. A strike consumes
        // all progress observed so far, so a single burst cannot justify several strikes.
        let (_, since) = self.cursor.get_or_insert_with(|| (pool.clone(), batch_start));
        *since = pool.peer_after(device, *since)?;
        self.count.record()
    }
}
