//! Virtual background PIDs. These numbers must never reach host kill(2).
use anyhow::Result;
use std::sync::{Mutex, PoisonError};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::{mpsc, watch};
#[derive(Clone)]
pub struct Processes {
    next: Arc<AtomicU64>,
    active: Arc<Mutex<HashMap<u64, Process>>>,
}
struct Process {
    signal: mpsc::Sender<String>,
    done: watch::Receiver<bool>,
}
impl Default for Processes {
    fn default() -> Self {
        Self {
            next: Arc::new(AtomicU64::new(1000)),
            active: Arc::default(),
        }
    }
}
impl Processes {
    pub(crate) fn owned(&self, pid: u64) -> Registration {
        Registration {
            processes: self.clone(),
            pid,
        }
    }
    pub async fn register(&self) -> (u64, mpsc::Receiver<String>, watch::Sender<bool>) {
        let pid = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(16);
        let (done, wait) = watch::channel(false);
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(pid, Process { signal: tx, done: wait });
        (pid, rx, done)
    }
    pub async fn signal(&self, pid: u64, signal: String, wait: bool) -> Result<bool> {
        let Some((tx, mut done)) = self
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&pid)
            .map(|p| (p.signal.clone(), p.done.clone()))
        else {
            return Ok(false);
        };
        if tx.send(signal).await.is_err() {
            return Ok(false);
        }
        if wait {
            let _ = done.wait_for(|done| *done).await;
        }
        Ok(true)
    }
    pub async fn completed(&self, pid: u64) {
        self.active.lock().unwrap_or_else(PoisonError::into_inner).remove(&pid);
    }
    pub async fn active(&self) -> Vec<u64> {
        let mut ids = self
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keys()
            .copied()
            .collect::<Vec<_>>();
        ids.sort();
        ids
    }
}

pub(crate) struct Registration {
    processes: Processes,
    pid: u64,
}
impl Drop for Registration {
    fn drop(&mut self) {
        self.processes
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelled_registration_does_not_survive_before_background_spawn() -> Result<()> {
        let processes = Processes::default();
        let (pid, _signals, _done) = processes.register().await;
        let registration = processes.owned(pid);
        let pending = async move {
            let _registration = registration;
            std::future::pending::<()>().await;
        };
        drop(Box::pin(pending));
        assert!(processes.active().await.is_empty());
        assert!(!processes.signal(pid, "INT".into(), true).await?);
        Ok(())
    }
}
