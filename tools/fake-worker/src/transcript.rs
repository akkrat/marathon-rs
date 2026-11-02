//! Ordered, lossless evidence owned by one worker run.
use anyhow::Result;
use base64::{Engine, prelude::BASE64_STANDARD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{io::Write, path::Path, sync::Arc};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct Transcript {
    writer: Arc<Mutex<Writer>>,
    #[cfg(test)]
    pause: Arc<std::sync::Mutex<Option<tests::Pause>>>,
}
struct Writer {
    file: std::fs::File,
    error: Option<String>,
    seq: u64,
    run: String,
}
impl Transcript {
    pub async fn create(path: &Path, run: String) -> Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true).mode(0o600);
        let path = path.to_owned();
        let file = tokio::task::spawn_blocking(move || opts.open(path)).await??;
        Ok(Self {
            writer: Arc::new(Mutex::new(Writer {
                file,
                error: None,
                seq: 0,
                run,
            })),
            #[cfg(test)]
            pause: Arc::default(),
        })
    }
    pub async fn event(&self, connection: u64, channel: Option<u32>, event: &str, detail: Value) -> Result<()> {
        let mut w = self.writer.clone().lock_owned().await;
        w.check()?;
        let row = json!({"schema":1,"run":w.run,"seq":w.seq,"connection":connection,"channel":channel,"event":event,"detail":detail});
        let mut bytes = serde_json::to_vec(&row)?;
        bytes.push(b'\n');
        #[cfg(test)]
        let pause = {
            let mut pause = self.pause.lock().expect("test pause lock");
            if pause.as_ref().is_some_and(|p| p.event == event) {
                pause.take()
            } else {
                None
            }
        };
        #[cfg(test)]
        let _pending = tests::Pending(pause.as_ref().map(|p| p.dropped.clone()));
        // Move the lease with the entire append. Cancellation can discard the
        // waiter, but cannot interrupt a record or strand its writer mutex.
        tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            let result = match pause {
                Some(pause) => pause.write(&mut w.file, &bytes),
                None => w.file.write_all(&bytes),
            };
            #[cfg(not(test))]
            let result = w.file.write_all(&bytes);
            match result {
                Ok(()) => w.seq += 1,
                Err(e) => w.error = Some(e.to_string()),
            }
            w.check()
        })
        .await?
    }
    pub async fn flush(&self) -> Result<()> {
        // Acquiring the lease joins even an append whose caller was cancelled.
        // A failed detached append must make final cleanup fail, too.
        self.writer.lock().await.check()
    }
}
impl Writer {
    fn check(&self) -> Result<()> {
        if let Some(error) = &self.error {
            anyhow::bail!("transcript write failed: {error}");
        }
        Ok(())
    }
}
pub fn bytes(data: &[u8]) -> Value {
    json!({"base64":BASE64_STANDARD.encode(data),"size":data.len(),"sha256":hash(data)})
}
pub fn hash(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};
    use tokio::{sync::Notify, time::timeout};

    pub(super) struct Pause {
        pub event: String,
        pub entered: Arc<Notify>,
        pub dropped: Arc<Notify>,
        pub release: mpsc::Receiver<()>,
        pub fail: bool,
    }
    impl Pause {
        pub(super) fn write(self, file: &mut std::fs::File, bytes: &[u8]) -> std::io::Result<()> {
            let middle = bytes.len() / 2;
            file.write_all(&bytes[..middle])?;
            self.entered.notify_one();
            self.release
                .recv_timeout(Duration::from_secs(5))
                .map_err(std::io::Error::other)?;
            if self.fail {
                return Err(std::io::Error::other("injected transcript write failure"));
            }
            file.write_all(&bytes[middle..])
        }
    }
    pub(super) struct Pending(pub Option<Arc<Notify>>);
    impl Drop for Pending {
        fn drop(&mut self) {
            if let Some(dropped) = &self.0 {
                dropped.notify_one();
            }
        }
    }
    pub(crate) struct PausedAppend {
        pub entered: Arc<Notify>,
        pub dropped: Arc<Notify>,
        pub release: mpsc::Sender<()>,
    }
    impl Transcript {
        pub(crate) fn pause_append(&self, event: &str, fail: bool) -> PausedAppend {
            let entered = Arc::new(Notify::new());
            let dropped = Arc::new(Notify::new());
            let (release, rx) = mpsc::channel();
            *self.pause.lock().expect("test pause lock") = Some(Pause {
                event: event.into(),
                entered: entered.clone(),
                dropped: dropped.clone(),
                release: rx,
                fail,
            });
            PausedAppend {
                entered,
                dropped,
                release,
            }
        }
    }

    #[tokio::test]
    async fn cancelled_append_finishes_record_and_preserves_sequence() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("events.jsonl");
        let log = Transcript::create(&path, "cancel".into()).await?;
        let pause = log.pause_append("output", false);
        let mut event = Box::pin(log.event(1, Some(1), "output", bytes(&vec![0xff; 65536])));
        tokio::select! {
            biased;
            result = &mut event => anyhow::bail!("append did not pause: {result:?}"),
            _ = pause.entered.notified() => (),
        }
        drop(event);
        timeout(Duration::from_secs(1), pause.dropped.notified()).await?;
        pause.release.send(())?;
        log.event(1, Some(1), "exit_signal", json!({"signal":"TERM"})).await?;
        log.flush().await?;
        let text = tokio::fs::read_to_string(path).await?;
        let rows: Vec<Value> = text.lines().map(serde_json::from_str).collect::<Result<_, _>>()?;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["detail"], bytes(&vec![0xff; 65536]));
        assert_eq!(rows[0]["seq"], 0);
        assert_eq!(rows[1]["seq"], 1);
        Ok(())
    }

    #[tokio::test]
    async fn suspended_append_waiter_cannot_block_another_event() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let log = Transcript::create(&dir.path().join("events.jsonl"), "suspended".into()).await?;
        let pause = log.pause_append("output", false);
        let mut event = Box::pin(log.event(1, Some(1), "output", json!({})));
        tokio::select! {
            biased;
            result = &mut event => anyhow::bail!("append did not pause: {result:?}"),
            _ = pause.entered.notified() => (),
        }
        pause.release.send(())?;
        // Signal/ignored-signal/stdin branches may log while an output future
        // remains pinned and unpolled. Its append must make independent progress.
        timeout(
            Duration::from_secs(1),
            log.event(1, Some(1), "signal_ignored", json!({})),
        )
        .await??;
        event.await?;
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_append_failure_is_reported_by_flush_and_later_events() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let log = Transcript::create(&dir.path().join("events.jsonl"), "failure".into()).await?;
        let pause = log.pause_append("output", true);
        let mut event = Box::pin(log.event(1, Some(1), "output", json!({})));
        tokio::select! {
            biased;
            result = &mut event => anyhow::bail!("append did not pause: {result:?}"),
            _ = pause.entered.notified() => (),
        }
        drop(event);
        pause.release.send(())?;
        assert!(log.flush().await.is_err());
        assert!(
            log.event(0, None, "cleanup", json!({"outcome":"completed"}))
                .await
                .is_err()
        );
        Ok(())
    }
}
