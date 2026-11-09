//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/listener/LogListener.kt
//! and report/logs/LogWriter.kt, report/attachment/, device/file/FileTransferExtensions.kt.
use marathon_error::FailureKind;
mod event_support;

use event_support::*;
use jcompat::{
    file_manager::FileManager,
    logging::{Level, Logger, MarathonLogging},
};
use marathon_core::{
    artifacts::*,
    error::*,
    executor::{
        CoreExecutor,
        hooks::{ClockMode, RuntimeHooks, Tape},
    },
    listener::*,
    model::*,
    output::*,
    runtime::*,
    time::ReplayHooks,
    transfer::TransferMeasurement,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "marathon-event-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        checked(std::fs::create_dir_all(&path));
        Self(path)
    }
    fn path(&self, name: &str) -> PathBuf {
        PathBuf::from(self.0.join(name).to_str().unwrap())
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn oracle(id: &str) -> Value {
    include_str!("../../../fixtures/oracle/event-pipeline/probes.jsonl")
        .lines()
        .map(|s| serde_json::from_str::<Value>(s).unwrap())
        .find(|r| r["id"] == id)
        .unwrap()
}
fn assert_worker() {
    assert_ne!(std::thread::current().name(), Some("marathon-core"));
}
fn failure() -> Failure {
    marathon_core::executor::shared::failure(FailureKind::Io, "save failed")
}
fn target() -> LogTarget {
    LogTarget {
        pool: pool("myPool"),
        batch_id: "batch".into(),
        device: device(),
        test: None,
    }
}
fn output(io: Arc<dyn VendorRuntime>, path: PathBuf) -> Arc<FileManagerOutput> {
    Arc::new(FileManagerOutput::new(
        io,
        FileManager::new(0, 0, &path.to_string_lossy(), ""),
        path,
        MarathonLogging::default().logger("FileManager", None),
        Arc::new(|_, _, _, _| assert_worker()),
    ))
}
#[derive(Default)]
struct Producer {
    listeners: Mutex<Vec<Arc<dyn LineListener>>>,
}
impl LogProducer for Producer {
    fn add_line_listener(&self, l: Arc<dyn LineListener>) -> VendorResult<()> {
        assert_worker();
        self.listeners.lock().unwrap().push(l);
        Ok(())
    }
    fn remove_line_listener(&self, l: &Arc<dyn LineListener>) -> VendorResult<()> {
        assert_worker();
        let mut list = self.listeners.lock().unwrap();
        if let Some(i) = list.iter().position(|candidate| Arc::ptr_eq(candidate, l)) {
            list.remove(i);
        }
        Ok(())
    }
}
impl Producer {
    async fn line(&self, line: String) {
        let list = self.listeners.lock().unwrap().clone();
        for listener in list {
            listener.on_line(&line).await.unwrap();
        }
    }
}
#[test]
fn logs_keep_preamble_reset_tail_partition_blank_lines_and_arc_removal() {
    let temp = Arc::new(Temp::new());
    let retained = temp.clone();
    CoreExecutor::run(move |core| {
        Box::pin(async move {
            let temp = retained;
            let files = output(core.vendor(), temp.path("out"));
            let writer = Arc::new(FileLogWriter::new(core.vendor(), files.clone()));
            let producer = Arc::new(Producer::default());
            let io = CountingRuntime::new(core.vendor());
            let listener = LogListener::new(io.clone(), producer.clone(), writer, target(), None);
            let provider: Arc<dyn AttachmentProvider> = listener.clone();
            let collector = AttachmentCollector::new(std::slice::from_ref(&provider)).unwrap();
            let test = test("test");
            listener.before_test_run().await.unwrap();
            let spawns = io.spawns.load(Ordering::SeqCst);
            producer.line("preamble".into()).await;
            assert_eq!(io.spawns.load(Ordering::SeqCst), spawns);
            listener.test_started(&test).await.unwrap();
            producer.line("one".into()).await;
            producer.line("".into()).await;
            listener.test_ended(&test).await.unwrap();
            producer.line("tail".into()).await;
            listener.after_test_run().await.unwrap();
            let attachments = collector.get(&test).unwrap();
            assert_eq!(attachments.len(), 1);
            assert_eq!(attachments[0].kind, AttachmentType::Log);
            assert_eq!(attachments[0].name, Some("log".into()));
            let test_path = attachments[0].file.clone();
            let batch = files
                .create_file(&FileTarget {
                    kind: FileType::Log,
                    pool: pool("myPool"),
                    device: device(),
                    test: None,
                    test_batch_id: Some("batch".into()),
                    id: None,
                    chunk: None,
                })
                .await?;
            assert!(batch.to_string_lossy().into_owned().ends_with("batch.batch"));
            assert!(producer.listeners.lock().unwrap().is_empty());
            // Synchronous verification belongs to a blocking task as production writes do.
            let (tx, rx) = tokio::sync::oneshot::channel();
            let worker = core.vendor().spawn_blocking(Box::new(move || {
                let bytes = checked(std::fs::read(test_path.clone()));
                let tail = checked(std::fs::read(batch.clone()));
                tx.send((bytes, tail)).unwrap();
                Ok(())
            }));
            worker.join().await.unwrap();
            let (bytes, tail) = rx.await.unwrap();
            assert_eq!(json!(bytes), oracle("logs")["testBytes"]);
            assert_eq!(json!(tail), oracle("logs")["batchBytes"]);
            listener.after_test_run().await.unwrap(); // Tail is not reset by afterTestRun.
            Ok(())
        })
    })
    .unwrap();
}
struct RecordingWriter {
    saved: Mutex<Vec<(bool, Vec<String>)>>,
    fail: bool,
    threads: Mutex<Vec<String>>,
}
impl LogWriter for RecordingWriter {
    fn save_logs<'a>(&'a self, target: &'a LogTarget, logs: &'a [String]) -> ServiceFuture<'a, PathBuf> {
        Box::pin(async move {
            assert_worker();
            self.threads
                .lock()
                .unwrap()
                .push(std::thread::current().name().unwrap_or("").into());
            self.saved.lock().unwrap().push((target.test.is_some(), logs.to_vec()));
            if self.fail {
                Err(failure().into())
            } else {
                Ok(PathBuf::from("/nonexistent/log"))
            }
        })
    }
    fn append_logs<'a>(
        &'a self,
        _: &'a DevicePoolId,
        _: &'a DeviceInfo,
        _: &'a Test,
        _: &'a str,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}
#[test]
fn log_save_failure_clears_before_await_and_suppresses_attachments() {
    CoreExecutor::run(|core| {
        Box::pin(async move {
            let writer = Arc::new(RecordingWriter {
                saved: Mutex::default(),
                fail: true,
                threads: Mutex::default(),
            });
            let listener = LogListener::new(
                core.vendor(),
                Arc::new(Producer::default()),
                writer.clone(),
                target(),
                None,
            );
            let provider: Arc<dyn AttachmentProvider> = listener.clone();
            let collector = AttachmentCollector::new(std::slice::from_ref(&provider)).unwrap();
            let t = test("test");
            listener.on_line("lost").await.unwrap();
            assert_eq!(listener.test_ended(&t).await, Err(VendorError::Other(failure())));
            listener.test_ended(&t).await.unwrap();
            assert_eq!(writer.saved.lock().unwrap().len(), 1);
            assert!(collector.get(&t).is_none());
            listener.close().await.unwrap();
            Ok(())
        })
    })
    .unwrap();
}
#[test]
fn log_tail_interleavings_are_scripted_and_close_does_not_flush() {
    for line_before_end in [true, false] {
        CoreExecutor::run(move |core| {
            Box::pin(async move {
                let writer = Arc::new(RecordingWriter {
                    saved: Mutex::default(),
                    fail: false,
                    threads: Mutex::default(),
                });
                let producer = Arc::new(Producer::default());
                let listener = LogListener::new(
                    core.vendor(),
                    producer.clone(),
                    writer.clone(),
                    target(),
                    Some("custom".into()),
                );
                let t = test("test");
                listener.before_test_run().await.unwrap();
                listener.test_started(&t).await.unwrap();
                if line_before_end {
                    producer.line("tail".into()).await;
                }
                listener.test_ended(&t).await.unwrap();
                if !line_before_end {
                    producer.line("tail".into()).await;
                }
                listener.close().await.unwrap();
                assert_eq!(writer.saved.lock().unwrap().len(), usize::from(line_before_end));
                listener.after_test_run().await.unwrap();
                let saved = writer.saved.lock().unwrap();
                assert_eq!(saved.len(), 1);
                assert_eq!(saved[0], (line_before_end, vec![String::from("tail\n")]));
                Ok(())
            })
        })
        .unwrap();
    }
}
#[test]
fn log_writer_encoding_append_and_output_paths_use_real_off_core_files() {
    let temp = Arc::new(Temp::new());
    let retained = temp.clone();
    CoreExecutor::run(move |core| {
        Box::pin(async move {
            let temp = retained;
            let files = output(core.vendor(), temp.path("out"));
            let writer = FileLogWriter::new(core.vendor(), files.clone());
            let t = test("test");
            let bad = String::from_utf16_lossy(&[97, 0xd800, 98]);
            let target = LogTarget {
                test: Some(t.clone()),
                batch_id: "utf".into(),
                ..target()
            };
            let saved = writer.save_logs(&target, &[bad.clone(), "".into()]).await?;
            writer.append_logs(&pool("myPool"), &device(), &t, &bad).await?;
            let appended = files
                .create_file(&FileTarget {
                    kind: FileType::Log,
                    pool: pool("myPool"),
                    device: device(),
                    test: Some(t),
                    test_batch_id: None,
                    id: None,
                    chunk: None,
                })
                .await?;
            let (tx, rx) = tokio::sync::oneshot::channel();
            let task = core.vendor().spawn_blocking(Box::new(move || {
                tx.send((
                    checked(std::fs::read(saved.clone())),
                    checked(std::fs::read(appended.clone())),
                ))
                .unwrap();
                Ok(())
            }));
            task.join().await.unwrap();
            let (a, b) = rx.await.unwrap();
            assert_eq!(a, "a�b\n".as_bytes());
            assert_eq!(b, "a�b\n".as_bytes());
            let p = files
                .create_file(&FileTarget {
                    kind: FileType::ScreenshotWebp,
                    pool: pool("raw/pool"),
                    device: device(),
                    test: None,
                    test_batch_id: None,
                    id: None,
                    chunk: None,
                })
                .await?;
            assert!(
                p.to_string_lossy()
                    .into_owned()
                    .ends_with("screenshot/raw/pool/xxyyzz.jpg")
            );
            let (tx, rx) = tokio::sync::oneshot::channel();
            let task = core.vendor().spawn_blocking(Box::new(move || {
                let path = p.clone();
                tx.send((path.exists(), path.parent().unwrap().is_dir())).unwrap();
                Ok(())
            }));
            task.join().await.unwrap();
            assert_eq!(rx.await.unwrap(), (false, true));
            Ok(())
        })
    })
    .unwrap();
}
struct Attach {
    calls: Arc<Mutex<Vec<&'static str>>>,
    name: &'static str,
    fail: bool,
    register: Option<(Arc<AttachmentProviderDelegate>, Arc<dyn AttachmentListener>)>,
}
impl AttachmentListener for Attach {
    fn on_attachment(&self, _: &Test, _: Attachment) -> VendorResult<()> {
        self.calls.lock().unwrap().push(self.name);
        if let Some((provider, listener)) = &self.register {
            provider.register_listener(listener.clone())?;
        }
        if self.fail {
            Err(VendorError::Other(failure()))
        } else {
            Ok(())
        }
    }
}
fn attachment() -> Attachment {
    Attachment {
        file: PathBuf::from("/missing"),
        kind: AttachmentType::Log,
        name: None,
    }
}
#[test]
fn attachment_duplicates_metadata_keys_snapshot_failure_and_non_draining_lookup() {
    let provider = Arc::new(AttachmentProviderDelegate::default());
    let collector = AttachmentCollector::new(&[provider.clone(), provider.clone()]).unwrap();
    let mut t = test("test");
    provider.on_attachment(&t, attachment()).unwrap();
    t.meta_properties.push(MetaProperty {
        name: "annotation".into(),
        values: MetaMap::default(),
    });
    provider.on_attachment(&t, attachment()).unwrap();
    assert_eq!(json!(collector.get(&t).unwrap().len()), oracle("attachments")["count"]);
    assert_eq!(collector.get(&t).unwrap().len(), 4);
    assert!(collector.get(&test("unknown")).is_none());
    let calls = Arc::new(Mutex::new(vec![]));
    let provider = Arc::new(AttachmentProviderDelegate::default());
    let extra = Arc::new(Attach {
        calls: calls.clone(),
        name: "extra",
        fail: false,
        register: None,
    });
    provider
        .register_listener(Arc::new(Attach {
            calls: calls.clone(),
            name: "first",
            fail: false,
            register: Some((provider.clone(), extra)),
        }))
        .unwrap();
    provider.on_attachment(&t, attachment()).unwrap();
    assert_eq!(*calls.lock().unwrap(), ["first"]);
    provider.on_attachment(&t, attachment()).unwrap();
    assert_eq!(*calls.lock().unwrap(), ["first", "first", "extra"]);
    let provider = AttachmentProviderDelegate::default();
    provider
        .register_listener(Arc::new(Attach {
            calls: calls.clone(),
            name: "bad",
            fail: true,
            register: None,
        }))
        .unwrap();
    provider
        .register_listener(Arc::new(Attach {
            calls: calls.clone(),
            name: "never",
            fail: false,
            register: None,
        }))
        .unwrap();
    assert!(provider.on_attachment(&t, attachment()).is_err());
    assert_eq!(calls.lock().unwrap().last(), Some(&"bad"));
}
#[test]
fn attachment_empty_follows_stat_including_directories() {
    let temp = Arc::new(Temp::new());
    checked(std::fs::write(temp.0.join("empty"), b""));
    checked(std::fs::write(temp.0.join("nonempty"), b"x"));
    checked(std::fs::create_dir(temp.0.join("directory")));
    let retained = temp.clone();
    CoreExecutor::run(move |core| {
        Box::pin(async move {
            let temp = retained;
            let files = LocalAttachmentFiles { io: core.vendor() };
            for name in ["missing", "empty", "nonempty", "directory"] {
                let a = Attachment {
                    file: temp.path(name),
                    kind: AttachmentType::Log,
                    name: None,
                };
                assert_eq!(
                    json!(files.is_empty(&a).await.unwrap()),
                    oracle("empty")[if name == "directory" { "directoryEmpty" } else { name }]
                );
            }
            let invalid = Attachment {
                file: PathBuf::from("invalid\0path"),
                kind: AttachmentType::Log,
                name: None,
            };
            assert!(files.is_empty(&invalid).await.unwrap());
            Ok(())
        })
    })
    .unwrap();
}
struct Line {
    calls: Arc<Mutex<Vec<String>>>,
    name: &'static str,
    fail: bool,
}
impl LineListener for Line {
    fn on_line<'a>(&'a self, _: &'a str) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            assert_worker();
            self.calls.lock().unwrap().push(self.name.into());
            if self.fail {
                Err(VendorError::Other(failure()))
            } else {
                Ok(())
            }
        })
    }
    fn close(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            assert_worker();
            self.calls.lock().unwrap().push(format!("close-{}", self.name));
            if self.fail {
                Err(VendorError::Other(failure()))
            } else {
                Ok(())
            }
        })
    }
}
#[test]
fn composite_line_and_close_stop_at_first_failure() {
    CoreExecutor::run(|core| {
        Box::pin(async move {
            let calls = Arc::new(Mutex::new(vec![]));
            let composite = CompositeLineListener {
                io: core.vendor(),
                listeners: [("a", false), ("bad", true), ("never", false)]
                    .into_iter()
                    .map(|(name, fail)| {
                        Arc::new(Line {
                            calls: calls.clone(),
                            name,
                            fail,
                        }) as Arc<dyn LineListener>
                    })
                    .collect(),
            };
            assert!(composite.on_line("line").await.is_err());
            assert!(composite.close().await.is_err());
            assert_eq!(*calls.lock().unwrap(), ["a", "bad", "close-a", "close-bad"]);
            Ok(())
        })
    })
    .unwrap();
}
#[test]
fn file_transfer_finally_and_folder_success_use_tapes_and_integer_kilobytes() {
    let temp = Arc::new(Temp::new());
    checked(std::fs::write(temp.0.join("small"), vec![0u8; 999]));
    checked(std::fs::create_dir_all(temp.0.join("folder/sub")));
    checked(std::fs::write(temp.0.join("folder/sub/file"), vec![0u8; 1500]));
    let retained = temp.clone();
    CoreExecutor::run_virtual(move |core, driver| {
        Box::pin(async move {
            let temp = retained;
            let mut tape = Tape {
                wall: ClockMode::Frozen(0),
                ..Tape::default()
            };
            tape.reads.insert(
                "core.FileTransferExtensions.measureFileTransfer".into(),
                vec![0, 1000, 2000, 3000, 4000, 5000],
            );
            tape.reads.insert(
                "core.FileTransferExtensions.measureFolderTransfer".into(),
                vec![0, 1000, 2000],
            );
            let hooks = RuntimeHooks::taped(tape, &core.clock());
            driver.set_hooks(hooks.clone());
            let messages = Arc::new(Mutex::new(vec![]));
            let recorded = messages.clone();
            let measure = TransferMeasurement {
                io: core.vendor(),
                clock: hooks.clone(),
                logger: MarathonLogging::default().logger("Device", None),
                serial: "serial".into(),
                log: Arc::new(move |_: &Logger, level: Level, message: &str, _| {
                    assert_worker();
                    assert_eq!(level, Level::Debug);
                    recorded.lock().unwrap().push(message.to_owned());
                }),
            };
            let value = String::from("borrowed transfer result");
            assert_eq!(
                measure
                    .measure_file_transfer(
                        temp.path("small"),
                        Box::pin(async {
                            assert_eq!(std::thread::current().name(), Some("marathon-core"));
                            Ok(value.as_str())
                        })
                    )
                    .await,
                Ok(value.as_str())
            );
            assert_eq!(
                measure
                    .measure_file_transfer(
                        temp.path("small"),
                        Box::pin(async { Err::<(), _>(VendorError::Transfer(failure())) })
                    )
                    .await,
                Err(VendorError::Transfer(failure()))
            );
            measure
                .measure_file_transfer(temp.path("missing"), Box::pin(async { Ok(()) }))
                .await
                .unwrap();
            measure
                .measure_folder_transfer(temp.path("folder"), Box::pin(async { Ok(()) }))
                .await
                .unwrap();
            assert!(
                measure
                    .measure_folder_transfer(
                        temp.path("folder"),
                        Box::pin(async { Err::<(), _>(VendorError::Other(failure())) })
                    )
                    .await
                    .is_err()
            );
            assert_eq!(
                *messages.lock().unwrap(),
                [
                    "Transferred small to/from serial. 0.00 KB/s (999 bytes in 1.0000)",
                    "Transferred small to/from serial. 0.00 KB/s (999 bytes in 1.0000)",
                    "Transferred folder with 2 dirs and 1 files to/from serial. 1.00 KB/s (1500 bytes in 1.0000)"
                ]
            );
            let expected = oracle("transfer");
            let actual = messages.lock().unwrap();
            assert_eq!(actual.len(), expected["messages"].as_array().unwrap().len());
            for (i, message) in actual.iter().enumerate().take(2) {
                assert_eq!(
                    message.split(" bytes in ").next(),
                    expected["messages"][i].as_str().unwrap().split(" bytes in ").next()
                );
            }
            assert!(
                expected["messages"][2]
                    .as_str()
                    .unwrap()
                    .starts_with("Transferred folder with 2 dirs and 1 files to/from serial. ")
            );
            hooks.verify_consumed().unwrap();
            Ok(())
        })
    })
    .unwrap();
}

#[test]
fn dropping_transfer_cancels_borrowed_body_without_stat_or_log() {
    use std::sync::atomic::AtomicBool;
    let temp = Arc::new(Temp::new());
    checked(std::fs::write(temp.0.join("file"), vec![0u8; 1500]));
    CoreExecutor::run(move |core| {
        Box::pin(async move {
            for folder in [false, true] {
                let label = if folder {
                    "core.FileTransferExtensions.measureFolderTransfer"
                } else {
                    "core.FileTransferExtensions.measureFileTransfer"
                };
                let mut tape = Tape::default();
                tape.reads.insert(label.into(), vec![10]);
                let hooks = RuntimeHooks::taped(tape, &core.clock());
                let io = CountingRuntime::new(core.vendor());
                let messages = Arc::new(Mutex::new(vec![]));
                let recorded = messages.clone();
                let measure = TransferMeasurement {
                    io: io.clone(),
                    clock: hooks.clone(),
                    logger: MarathonLogging::default().logger("Device", None),
                    serial: "serial".into(),
                    log: Arc::new(move |_, _, message, _| {
                        recorded.lock().unwrap().push(message.to_owned());
                    }),
                };
                let dropped = AtomicBool::new(false);
                let completed = AtomicBool::new(false);
                let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
                let (release_tx, release_rx) = tokio::sync::oneshot::channel();
                let body = Box::pin(async {
                    let _guard = DropFlag(&dropped);
                    checked(entered_tx.send(()));
                    checked(release_rx.await);
                    completed.store(true, Ordering::SeqCst);
                    Ok(())
                });
                let mut caller: VendorFuture<'_, ()> = if folder {
                    Box::pin(measure.measure_folder_transfer(temp.path(""), body))
                } else {
                    Box::pin(measure.measure_file_transfer(temp.path("file"), body))
                };
                match futures_util::future::select(caller.as_mut(), entered_rx).await {
                    futures_util::future::Either::Left((result, _)) => {
                        panic!("body returned before release: {result:?}")
                    }
                    futures_util::future::Either::Right((entered, _)) => checked(entered),
                }
                drop(caller);
                assert!(dropped.load(Ordering::SeqCst));
                assert!(release_tx.send(()).is_err());
                assert!(!completed.load(Ordering::SeqCst));
                assert!(messages.lock().unwrap().is_empty());
                assert_eq!(io.spawns.load(Ordering::SeqCst), 1);
                assert_eq!(io.blocking.load(Ordering::SeqCst), 0);
                hooks.verify_consumed().unwrap();
            }
            Ok(())
        })
    })
    .unwrap();
}
#[test]
fn output_materialization_retains_diagnostic_order_and_ignored_mkdirs() {
    let temp = Arc::new(Temp::new());
    let retained = temp.clone();
    CoreExecutor::run(move |core| {
        Box::pin(async move {
            let temp = retained;
            let out = temp.path("out");
            let original = temp.0.join("out/logs/pool/xxyyzz");
            let seen = Arc::new(Mutex::new(vec![]));
            let capture = seen.clone();
            let expected_dir = original.clone();
            let io = core.vendor();
            let files = FileManagerOutput::new(
                io.clone(),
                FileManager::new(
                    (out.to_string_lossy().encode_utf16().count() + 3) as i32,
                    4,
                    &out.to_string_lossy(),
                    "",
                ),
                out.clone(),
                MarathonLogging::default().logger("FileManager", None),
                Arc::new(move |_, _, message, _| {
                    assert_worker();
                    capture
                        .lock()
                        .unwrap()
                        .push((message.to_owned(), expected_dir.is_dir()));
                }),
            );
            let path = files
                .create_file(&FileTarget {
                    kind: FileType::Log,
                    pool: pool("pool"),
                    device: device(),
                    test: Some(test("method")),
                    test_batch_id: Some("batch".into()),
                    id: None,
                    chunk: None,
                })
                .await?;
            assert_eq!(
                path.to_string_lossy().encode_utf16().count(),
                out.to_string_lossy().encode_utf16().count() + 3
            );
            let seen = seen.lock().unwrap().clone();
            assert_eq!(seen.len(), 2);
            assert!(seen[0].0.starts_with("File name length"));
            assert!(seen[1].0.starts_with("File path length"));
            assert!(seen.iter().all(|(_, created)| *created));
            let folder_root = temp.path("folder-out");
            let truncated = folder_root.clone();
            let created = Arc::new(Mutex::new(vec![]));
            let capture = created.clone();
            let checked = truncated.clone();
            let files = FileManagerOutput::new(
                io.clone(),
                FileManager::new(
                    folder_root.to_string_lossy().encode_utf16().count() as i32,
                    0,
                    &folder_root.to_string_lossy(),
                    "",
                ),
                folder_root,
                MarathonLogging::default().logger("FileManager", None),
                Arc::new(move |_, _, _, _| {
                    assert_worker();
                    capture.lock().unwrap().push(checked.clone().exists());
                }),
            );
            assert_eq!(
                files.create_folder(FolderType::Html, None, None).await?.as_os_str(),
                truncated.as_os_str()
            );
            assert_eq!(*created.lock().unwrap(), [false]);
            // createTestResultFile ignores File.mkdirs failure, and creates no file.
            let blocked = temp.path("blocked");
            let native = blocked.clone();
            let task = io.spawn_blocking(Box::new(move || {
                assert!(std::fs::write(native, b"x").is_ok());
                Ok(())
            }));
            task.join().await.unwrap();
            let files = output(io, blocked);
            let result = files.create_test_result_file("raw.json").await?;
            assert!(
                result
                    .to_string_lossy()
                    .into_owned()
                    .ends_with("blocked/test_result/raw.json")
            );
            assert!(
                files
                    .create_file(&FileTarget {
                        kind: FileType::Log,
                        pool: pool("pool"),
                        device: device(),
                        test: None,
                        test_batch_id: None,
                        id: None,
                        chunk: None
                    })
                    .await
                    .is_err()
            );
            Ok(())
        })
    })
    .unwrap();
}
#[test]
fn output_adapter_reuses_existing_jvm_path_tables() {
    let original: Vec<Value> = include_str!("../../../fixtures/oracle/jcompat/file_manager.jsonl")
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    let edge: Vec<Value> = include_str!("../../../fixtures/oracle/jcompat/file_manager_edges.jsonl")
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    // Only replace the table's sandbox prefix. The relative path remains its JVM result.
    let row = original
        .iter()
        .find(|r| {
            r["op"] == "createFile"
                && r["maxPath"] == 0
                && r["maxFilename"] == 255
                && r["args"]["test"].is_null()
                && r["args"]["testBatchId"].is_string()
                && r["args"]["fileType"] == "LOG"
        })
        .unwrap()
        .clone();
    let edge = edge
        .iter()
        .find(|r| r["id"] == "create_filename_normal_length_test")
        .unwrap()
        .clone();
    let temp = Arc::new(Temp::new());
    let retained = temp.clone();
    CoreExecutor::run(move |core| {
        Box::pin(async move {
            let temp = retained;
            for (index, row) in [row, edge].into_iter().enumerate() {
                let out = temp.path(&index.to_string());
                let files = output(core.vendor(), out.clone());
                let args = &row["args"];
                let mut d = device();
                d.serial_number = args["serial"].as_str().unwrap().into();
                let target = FileTarget {
                    kind: FileType::Log,
                    pool: pool(args["pool"].as_str().unwrap()),
                    device: d,
                    test: if index == 1 {
                        Some(Test {
                            pkg: "com".into(),
                            clazz: "example.Clazz".into(),
                            method: "method".into(),
                            meta_properties: vec![],
                        })
                    } else {
                        None
                    },
                    test_batch_id: args[if index == 1 { "batch" } else { "testBatchId" }]
                        .as_str()
                        .map(String::from),
                    id: args["id"].as_str().map(String::from),
                    chunk: args["chunk"].as_str().map(String::from),
                };
                let actual = files.create_file(&target).await?;
                let expected = if index == 1 {
                    String::from_utf16(
                        &row["result"]["units"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|u| u.as_u64().unwrap() as u16)
                            .collect::<Vec<_>>(),
                    )
                    .unwrap()
                } else {
                    row["result"]["ok"].as_str().unwrap().into()
                };
                let prefix = if index == 1 {
                    "/tmp/oracle-wp17/out"
                } else {
                    "/tmp/oracle-file-manager/out"
                };
                let relative = expected.strip_prefix(prefix).unwrap();
                assert_eq!(
                    actual.to_string_lossy().into_owned(),
                    format!("{}{relative}", out.to_string_lossy().into_owned())
                );
            }
            Ok(())
        })
    })
    .unwrap();
}
#[test]
fn native_directory_failure_retains_jvm_class_and_message() {
    let temp = Arc::new(Temp::new());
    assert!(std::fs::write(temp.0.join("not-dir"), b"x").is_ok());
    let retained = temp.clone();
    CoreExecutor::run(move |core| {
        Box::pin(async move {
            let files = output(core.vendor(), retained.path("not-dir"));
            let writer = FileLogWriter::new(core.vendor(), files);
            let target = LogTarget {
                pool: pool("pool"),
                test: Some(test("test")),
                ..target()
            };
            let error = writer.save_logs(&target, &["lost".into()]).await.unwrap_err();
            let OperationError::Failed(error) = error else {
                panic!("ordinary I/O failure")
            };
            let expected = oracle("log-reset-failure");
            assert_eq!(json!(error.kind.legacy_class()), expected["error"]);
            let message = error.message.unwrap();
            assert_eq!(
                json!(message.replace(retained.0.to_str().unwrap(), "<tmp>")),
                expected["message"]
            );
            assert!(error.cause.is_none());
            Ok(())
        })
    })
    .unwrap();
}

#[test]
fn log_writer_rejects_nul_filename_before_opening() {
    let temp = Temp::new();
    let failure = checked(CoreExecutor::run(move |core| {
        Box::pin(async move {
            let files = output(core.vendor(), temp.path("out"));
            let writer = FileLogWriter::new(core.vendor(), files);
            let mut target = target();
            target.batch_id = "batch\0tail".into();
            match writer.save_logs(&target, &["log".into()]).await {
                Err(OperationError::Failed(f)) => Ok(f),
                other => panic!("expected invalid path error, got {other:?}"),
            }
        })
    }));
    assert_eq!(failure.kind, marathon_error::FailureKind::InvalidPath);
    assert_eq!(failure.message.as_deref(), Some("Nul character not allowed"));
}
