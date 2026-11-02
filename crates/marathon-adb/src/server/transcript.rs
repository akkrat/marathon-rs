//! The request transcript: one JSON object per line, kept in memory and optionally written to a file.
//!
//! Record types (`"type"`):
//! - `request`: every service string, with the connection id and, after a transport switch, the serial;
//! - `shell`: a shell, exec or abb_exec command, how it was answered and the stdin payload it consumed;
//! - `sync`: one sync operation with its path and size;
//! - `event`: a device or server event (connect, disconnect, state, kill);
//! - `warning`: an unmatched command or a protocol error.

use std::io::{self, Write};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;

use serde_json::Value;
use tokio::sync::oneshot;

#[derive(Clone, Default)]
pub struct Transcript {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    records: Mutex<Vec<Value>>,
    file: Mutex<Option<FileWriter>>,
}

/// A plain thread writes the file so that no task blocks on disk I/O. The first write error is kept and
/// reported by [`Transcript::flush`] and [`Transcript::close`]; later lines are dropped.
struct FileWriter {
    tx: mpsc::Sender<Message>,
    error: Arc<Mutex<Option<String>>>,
    thread: Option<JoinHandle<()>>,
}

enum Message {
    Line(String),
    /// Answered once every earlier line has been written and flushed.
    Barrier(oneshot::Sender<()>),
}

impl Transcript {
    /// Also appends every record to `path` (truncated first), flushing after each line.
    pub fn with_file(path: &std::path::Path) -> io::Result<Transcript> {
        Transcript::with_writer(Box::new(std::fs::File::create(path)?))
    }

    /// Also writes every record, one JSON line each, to `out`.
    pub fn with_writer(out: Box<dyn Write + Send>) -> io::Result<Transcript> {
        let (tx, rx) = mpsc::channel::<Message>();
        let error = Arc::new(Mutex::new(None));
        let thread_error = error.clone();
        let thread = std::thread::Builder::new()
            .name("fake-adb-transcript".into())
            .spawn(move || {
                let mut out = io::BufWriter::new(out);
                for msg in rx {
                    match msg {
                        Message::Line(line) => {
                            if lock(&thread_error).is_some() {
                                continue;
                            }
                            if let Err(e) = writeln!(out, "{line}").and_then(|_| out.flush()) {
                                eprintln!("fake-adb: cannot write the transcript: {e}");
                                *lock(&thread_error) = Some(e.to_string());
                            }
                        }
                        Message::Barrier(ack) => {
                            let _ = ack.send(());
                        }
                    }
                }
            })?;
        let t = Transcript::default();
        *lock(&t.inner.file) = Some(FileWriter {
            tx,
            error,
            thread: Some(thread),
        });
        Ok(t)
    }

    pub fn record(&self, value: Value) {
        // Holding the records lock while sending keeps the file in the same order as memory.
        let mut records = lock(&self.inner.records);
        if let Some(w) = lock(&self.inner.file).as_ref() {
            // Only fails once the writer thread is gone, which flush and close report.
            let _ = w.tx.send(Message::Line(value.to_string()));
        }
        records.push(value);
    }

    pub fn warning(&self, message: impl Into<String>) {
        self.record(serde_json::json!({ "type": "warning", "message": message.into() }));
    }

    pub fn records(&self) -> Vec<Value> {
        lock(&self.inner.records).clone()
    }

    pub fn warnings(&self) -> usize {
        lock(&self.inner.records)
            .iter()
            .filter(|r| r["type"] == "warning")
            .count()
    }

    /// Waits until every record so far is in the file. Fails if a write failed.
    pub async fn flush(&self) -> io::Result<()> {
        let (ack, done) = oneshot::channel();
        let error = {
            let file = lock(&self.inner.file);
            let Some(w) = file.as_ref() else { return Ok(()) };
            if w.tx.send(Message::Barrier(ack)).is_err() {
                return Err(io::Error::other("the transcript writer has stopped"));
            }
            w.error.clone()
        };
        done.await
            .map_err(|_| io::Error::other("the transcript writer has stopped"))?;
        let e = lock(&error).clone();
        e.map_or(Ok(()), |e| {
            Err(io::Error::other(format!("cannot write the transcript: {e}")))
        })
    }

    /// Writes out everything and stops the writer; later records stay in memory only. Blocks until the
    /// file is complete.
    pub fn close(&self) -> io::Result<()> {
        let Some(mut w) = lock(&self.inner.file).take() else {
            return Ok(());
        };
        drop(w.tx);
        if let Some(t) = w.thread.take()
            && t.join().is_err()
        {
            return Err(io::Error::other("the transcript writer panicked"));
        }
        let e = lock(&w.error).clone();
        e.map_or(Ok(()), |e| {
            Err(io::Error::other(format!("cannot write the transcript: {e}")))
        })
    }
}

/// A poisoned lock only means another task panicked mid-update; the data is still usable for a fake.
pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Collects lines slowly, so that anything read before a barrier would be incomplete.
    struct SlowWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SlowWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            std::thread::sleep(Duration::from_millis(20));
            lock(&self.0).extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("disk full"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn flush_waits_for_a_slow_writer() {
        let out = Arc::new(Mutex::new(Vec::new()));
        let t = Transcript::with_writer(Box::new(SlowWriter(out.clone()))).unwrap();
        for i in 0..5 {
            t.record(serde_json::json!({"type": "request", "n": i}));
        }
        t.flush().await.unwrap();
        let text = String::from_utf8(lock(&out).clone()).unwrap();
        assert_eq!(text.lines().count(), 5, "{text}");
        let last: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        assert_eq!(last["n"], 4);
    }

    #[tokio::test]
    async fn write_failures_are_reported() {
        let t = Transcript::with_writer(Box::new(FailingWriter)).unwrap();
        t.record(serde_json::json!({"type": "request"}));
        t.warning("still recorded");
        let e = t.flush().await.unwrap_err();
        assert!(e.to_string().contains("disk full"), "{e}");
        assert_eq!(t.records().len(), 2);
        assert_eq!(t.warnings(), 1);
        assert!(t.close().unwrap_err().to_string().contains("disk full"));
    }

    #[test]
    fn close_drains_to_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let t = Transcript::with_file(&path).unwrap();
        t.record(serde_json::json!({"type": "request", "service": "host:version"}));
        t.warning("hello");
        t.close().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2);
        let first: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(first["service"], "host:version");
        // After close, records stay in memory only.
        t.warning("late");
        assert_eq!(t.records().len(), 3);
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);
    }
}
