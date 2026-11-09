//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/listener/LogListener.kt
//! and CompositeLineListener.kt, report/logs/LogWriter.kt,
//! report/attachment/AttachmentCollector.kt, AttachmentProviderDelegate.kt,
//! execution/Attachment.kt and io/FileManager.kt (service adapter).

use marathon_logging::{Level, Component};

use crate::{artifact_io::*, error::*, listener::*, model::*, output::*, runtime::*};
use jcompat::{
    file_manager::{FileManager, PathPlan},
    
};
use marathon_error::FailureKind;
use std::path::PathBuf;
use std::{
    collections::HashMap,
    io::Write,
    sync::{Arc, Mutex, Weak},
};

#[derive(Clone)]
pub struct FileManagerOutput {
    io: Arc<dyn VendorRuntime>,
    manager: Arc<FileManager>,
    output: PathBuf,
    logger: Component,
    log: ServiceLog,
}
impl FileManagerOutput {
    pub fn new(
        io: Arc<dyn VendorRuntime>,
        manager: FileManager,
        output: PathBuf,
        logger: Component,
        log: ServiceLog,
    ) -> Self {
        Self {
            io,
            manager: Arc::new(manager),
            output,
            logger,
            log,
        }
    }
    async fn materialize(&self, plan: PathPlan) -> OperationResult<PathBuf> {
        let (logger, log) = (self.logger.clone(), self.log.clone());
        blocking(self.io.as_ref(), move || {
            plan.materialize(|text| log(&logger, Level::ERROR, text, None))
                .map_err(|error| nio_error(error, &plan.directory))
        })
        .await
        .map_err(operation_error)
    }
}
impl OutputFiles for FileManagerOutput {
    fn output_dir(&self) -> &std::path::Path {
        &self.output
    }
    fn create_file<'a>(&'a self, target: &'a FileTarget) -> ServiceFuture<'a, PathBuf> {
        Box::pin(async move {
            let test = target.test.as_ref().map(|t| t.to_test_name('.', '#'));
            self.materialize(self.manager.plan_file(target.request(test.as_deref())))
                .await
        })
    }
    fn create_folder<'a>(
        &'a self,
        kind: FolderType,
        pool: Option<&'a DevicePoolId>,
        device: Option<&'a DeviceInfo>,
    ) -> ServiceFuture<'a, PathBuf> {
        Box::pin(async move {
            self.materialize(self.manager.plan_folder(
                kind,
                pool.map(|p| p.name.as_str()),
                device.map(|d| d.serial_number.as_str()),
            ))
            .await
        })
    }
    fn create_test_result_file<'a>(&'a self, filename: &'a str) -> ServiceFuture<'a, PathBuf> {
        Box::pin(async move { self.materialize(self.manager.plan_test_result_file(filename)).await })
    }
}
#[derive(Clone)]
pub struct FileLogWriter {
    io: Arc<dyn VendorRuntime>,
    files: Arc<dyn OutputFiles>,
}
impl FileLogWriter {
    pub fn new(io: Arc<dyn VendorRuntime>, files: Arc<dyn OutputFiles>) -> Self {
        Self { io, files }
    }
}
impl LogWriter for FileLogWriter {
    fn save_logs<'a>(&'a self, target: &'a LogTarget, logs: &'a [String]) -> ServiceFuture<'a, PathBuf> {
        let this = self.clone();
        let target = FileTarget {
            kind: FileType::Log,
            pool: target.pool.clone(),
            device: target.device.clone(),
            test: target.test.clone(),
            test_batch_id: Some(target.batch_id.clone()),
            id: None,
            chunk: None,
        };
        let logs = logs.to_vec();
        Box::pin(async move {
            on_io(
                self.io.as_ref(),
                Box::pin(async move {
                    let path = this.files.create_file(&target).await.map_err(vendor_error)?;
                    let mut text = String::default();
                    for (i, log) in logs.into_iter().enumerate() {
                        if i != 0 {
                            text.push('\n');
                        }
                        text.push_str(&log);
                    }
                    let output = path.clone();
                    blocking(this.io.as_ref(), move || {
                        validate_path(&output)?;
                        let mut file =
                            std::fs::File::create(&output).map_err(|error| file_open_error(error, &output))?;
                        file.write_all(text.as_bytes()).map_err(io_error)
                    })
                    .await?;
                    Ok(path)
                }),
            )
            .await
            .map_err(operation_error)
        })
    }
    fn append_logs<'a>(
        &'a self,
        pool: &'a DevicePoolId,
        device: &'a DeviceInfo,
        test: &'a Test,
        log: &'a str,
    ) -> ServiceFuture<'a, ()> {
        let this = self.clone();
        let target = FileTarget {
            kind: FileType::Log,
            pool: pool.clone(),
            device: device.clone(),
            test: Some(test.clone()),
            test_batch_id: None,
            id: None,
            chunk: None,
        };
        let mut text = log.to_owned();
        text.push('\n');
        Box::pin(async move {
            on_io(
                self.io.as_ref(),
                Box::pin(async move {
                    let path = this.files.create_file(&target).await.map_err(vendor_error)?;
                    blocking(this.io.as_ref(), move || {
                        validate_path(&path)?;
                        let mut file = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&path)
                            .map_err(|error| file_open_error(error, &path))?;
                        file.write_all(text.as_bytes()).map_err(io_error)
                    })
                    .await
                }),
            )
            .await
            .map_err(operation_error)
        })
    }
}
pub struct CompositeLineListener {
    pub io: Arc<dyn VendorRuntime>,
    pub listeners: Vec<Arc<dyn LineListener>>,
}
impl LineListener for CompositeLineListener {
    fn on_line<'a>(&'a self, line: &'a str) -> VendorFuture<'a, ()> {
        let (listeners, line) = (self.listeners.clone(), line.to_owned());
        Box::pin(async move {
            on_io(
                self.io.as_ref(),
                Box::pin(async move {
                    for listener in listeners {
                        listener.on_line(&line).await?;
                    }
                    Ok(())
                }),
            )
            .await
        })
    }
    fn close(&self) -> VendorFuture<'_, ()> {
        let listeners = self.listeners.clone();
        Box::pin(async move {
            on_io(
                self.io.as_ref(),
                Box::pin(async move {
                    for listener in listeners {
                        listener.close().await?;
                    }
                    Ok(())
                }),
            )
            .await
        })
    }
}
#[derive(Default)]
pub struct AttachmentProviderDelegate {
    listeners: Mutex<Vec<Arc<dyn AttachmentListener>>>,
}
impl AttachmentProvider for AttachmentProviderDelegate {
    fn register_listener(&self, listener: Arc<dyn AttachmentListener>) -> VendorResult<()> {
        self.listeners.lock().unwrap_or_else(|e| e.into_inner()).push(listener);
        Ok(())
    }
}
impl AttachmentProviderDelegate {
    pub fn on_attachment(&self, test: &Test, attachment: Attachment) -> VendorResult<()> {
        let listeners = self.listeners.lock().unwrap_or_else(|e| e.into_inner()).clone();
        for listener in listeners {
            listener.on_attachment(test, attachment.clone())?;
        }
        Ok(())
    }
}
#[derive(Default)]
pub struct AttachmentCollector {
    attachments: Mutex<HashMap<Test, Vec<Attachment>>>,
}
impl AttachmentCollector {
    pub fn new(providers: &[Arc<dyn AttachmentProvider>]) -> VendorResult<Arc<Self>> {
        let collector = Arc::new(Self::default());
        for provider in providers {
            provider.register_listener(collector.clone())?;
        }
        Ok(collector)
    }
    pub fn get(&self, test: &Test) -> Option<Vec<Attachment>> {
        self.attachments
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(test)
            .cloned()
    }
}
impl AttachmentListener for AttachmentCollector {
    fn on_attachment(&self, test: &Test, attachment: Attachment) -> VendorResult<()> {
        self.attachments
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(test.clone())
            .or_default()
            .push(attachment);
        Ok(())
    }
}
pub struct LocalAttachmentFiles {
    pub io: Arc<dyn VendorRuntime>,
}
impl AttachmentFiles for LocalAttachmentFiles {
    fn is_empty<'a>(&'a self, attachment: &'a Attachment) -> VendorFuture<'a, bool> {
        let path = attachment.file.clone();
        Box::pin(async move {
            blocking(self.io.as_ref(), move || {
                // File.exists/length both return zero-like values on failed stat,
                // including invalid paths. Directories keep their platform length.
                Ok(std::fs::metadata(&path).ok().is_none_or(|m| m.len() == 0))
            })
            .await
        })
    }
}
pub struct LogListener {
    io: Arc<dyn VendorRuntime>,
    producer: Arc<dyn LogProducer>,
    writer: Arc<dyn LogWriter>,
    target: LogTarget,
    attachments: AttachmentProviderDelegate,
    name: String,
    buffer: Mutex<String>,
    identity: Weak<Self>,
}
impl LogListener {
    pub fn new(
        io: Arc<dyn VendorRuntime>,
        producer: Arc<dyn LogProducer>,
        writer: Arc<dyn LogWriter>,
        target: LogTarget,
        name: Option<String>,
    ) -> Arc<Self> {
        Arc::new_cyclic(|identity| Self {
            io,
            producer,
            writer,
            target,
            attachments: AttachmentProviderDelegate::default(),
            name: name.unwrap_or_else(|| "log".into()),
            buffer: Mutex::default(),
            identity: identity.clone(),
        })
    }
    fn retained(&self) -> VendorResult<Arc<Self>> {
        self.identity.upgrade().ok_or_else(|| {
            VendorError::Other(crate::executor::shared::failure(
                FailureKind::IllegalState,
                "log listener identity expired",
            ))
        })
    }
    fn snapshot(&self, reset: bool) -> String {
        let mut buffer = self.buffer.lock().unwrap_or_else(|e| e.into_inner());
        if reset {
            std::mem::take(&mut *buffer)
        } else {
            buffer.clone()
        }
    }
}
impl AttachmentProvider for LogListener {
    fn register_listener(&self, listener: Arc<dyn AttachmentListener>) -> VendorResult<()> {
        self.attachments.register_listener(listener)
    }
}
impl LineListener for LogListener {
    fn on_line<'a>(&'a self, line: &'a str) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let mut buffer = self.buffer.lock().unwrap_or_else(|e| e.into_inner());
            buffer.push_str(line);
            buffer.push('\n');
            Ok(())
        })
    }
    fn close(&self) -> VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
impl TestRunListener for LogListener {
    fn before_test_run(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            let this = self.retained()?;
            on_io(
                self.io.as_ref(),
                Box::pin(async move { this.producer.add_line_listener(this.clone()) }),
            )
            .await
        })
    }
    fn test_started<'a>(&'a self, _test: &'a Test) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let this = self.retained()?;
            on_io(
                self.io.as_ref(),
                Box::pin(async move {
                    this.snapshot(true);
                    Ok(())
                }),
            )
            .await
        })
    }
    fn test_ended<'a>(&'a self, test: &'a Test) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let this = self.retained()?;
            let test = test.clone();
            on_io(
                self.io.as_ref(),
                Box::pin(async move {
                    let log = this.snapshot(true);
                    if !log.is_empty() {
                        let target = LogTarget {
                            pool: this.target.pool.clone(),
                            batch_id: this.target.batch_id.clone(),
                            device: this.target.device.clone(),
                            test: Some(test.clone()),
                        };
                        let file = this.writer.save_logs(&target, &[log]).await.map_err(vendor_error)?;
                        this.attachments.on_attachment(
                            &test,
                            Attachment {
                                file,
                                kind: AttachmentType::Log,
                                name: Some(this.name.to_owned()),
                            },
                        )?;
                    }
                    Ok(())
                }),
            )
            .await
        })
    }
    fn after_test_run(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            let this = self.retained()?;
            on_io(
                self.io.as_ref(),
                Box::pin(async move {
                    let listener: Arc<dyn LineListener> = this.clone();
                    this.producer.remove_line_listener(&listener)?;
                    let log = this.snapshot(false);
                    if !log.is_empty() {
                        let target = LogTarget {
                            pool: this.target.pool.clone(),
                            batch_id: this.target.batch_id.clone(),
                            device: this.target.device.clone(),
                            test: None,
                        };
                        this.writer.save_logs(&target, &[log]).await.map_err(vendor_error)?;
                    }
                    Ok(())
                }),
            )
            .await
        })
    }
}
