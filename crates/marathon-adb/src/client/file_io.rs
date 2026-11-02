//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/io/{AsyncFileReader,AsyncFileWriter}.kt`.
//! Copyright (C) 2021 Anton Malinskiy; Apache-2.0.
//!
//! P3: sequential I/O. Keep a completion handle before handing a request to a stream. After dropping
//! that stream, await `FileCompletion::close` on the vendor runtime: it drains an in-flight Tokio file
//! operation before releasing the file. Dropping only a `next()` future leaves the stream's work intact.

use super::AdbError;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{future::Future, pin::Pin};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

pub(crate) fn io_error(e: std::io::Error) -> AdbError {
    AdbError::Io(e.to_string())
}

#[derive(Clone, Default)]
pub struct FileCompletion(Arc<Mutex<FileState>>);
type Opening = Pin<Box<dyn Future<Output = Result<File, AdbError>> + Send>>;
#[derive(Default)]
struct FileState {
    file: Option<File>,
    opening: Option<Opening>,
}
impl FileState {
    async fn finish_open(&mut self) -> Result<(), AdbError> {
        if let Some(opening) = self.opening.as_mut() {
            let result = opening.await;
            self.opening = None;
            self.file = Some(result?);
        }
        Ok(())
    }
}
impl FileCompletion {
    pub async fn close(&self) -> Result<(), AdbError> {
        let mut file = self.0.lock().await;
        file.finish_open().await?;
        // adam only closes the channel; `flush` drains Tokio's in-flight operation without an fsync.
        if let Some(f) = file.file.as_mut() {
            f.flush().await.map_err(io_error)?;
        }
        file.file = None;
        Ok(())
    }
}

pub struct AsyncFileReader {
    completion: FileCompletion,
    pub size: u64,
}
impl AsyncFileReader {
    pub async fn prepare(path: &Path) -> Result<Self, AdbError> {
        let file = File::open(path).await.map_err(io_error)?;
        let size = file.metadata().await.map_err(io_error)?.len();
        Ok(Self {
            completion: FileCompletion(Arc::new(Mutex::new(FileState {
                file: Some(file),
                opening: None,
            }))),
            size,
        })
    }
    pub fn completion(&self) -> FileCompletion {
        self.completion.clone()
    }
    pub async fn read(&self, length: usize) -> Result<Vec<u8>, AdbError> {
        let mut guard = self.completion.0.lock().await;
        let file = guard
            .file
            .as_mut()
            .ok_or_else(|| AdbError::IllegalState("File reader is closed".into()))?;
        let mut bytes = vec![0; length];
        let n = file.read(&mut bytes).await.map_err(io_error)?;
        bytes.truncate(n);
        Ok(bytes)
    }
    pub async fn close(&self) -> Result<(), AdbError> {
        self.completion.close().await
    }
}

pub struct AsyncFileWriter {
    path: PathBuf,
    completion: FileCompletion,
}
impl AsyncFileWriter {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            completion: FileCompletion::default(),
        }
    }
    pub fn completion(&self) -> FileCompletion {
        self.completion.clone()
    }
    pub async fn start(&self) -> Result<(), AdbError> {
        let mut guard = self.completion.0.lock().await;
        if guard.file.is_none() && guard.opening.is_none() {
            let path = self.path.clone();
            guard.opening = Some(Box::pin(async move {
                if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    tokio::fs::create_dir_all(parent).await.map_err(io_error)?;
                }
                File::create(path).await.map_err(io_error)
            }));
        }
        guard.finish_open().await?;
        Ok(())
    }
    pub async fn write(&self, bytes: &[u8]) -> Result<(), AdbError> {
        let mut guard = self.completion.0.lock().await;
        let file = guard
            .file
            .as_mut()
            .ok_or_else(|| AdbError::IllegalState("File writer has not started".into()))?;
        file.write_all(bytes).await.map_err(io_error)?;
        // write_all can leave Tokio's blocking write pending. Flush before publishing progress.
        file.flush().await.map_err(io_error)
    }
    pub async fn close(&self) -> Result<(), AdbError> {
        self.completion.close().await
    }
}

/// The completion owner for a sequence that opens files as it goes (split install/directory push).
#[derive(Clone, Default)]
pub struct FileCompletions(Arc<Mutex<Vec<FileCompletion>>>);
impl FileCompletions {
    pub async fn register(&self, completion: FileCompletion) {
        self.0.lock().await.push(completion);
    }
    pub async fn close(&self) -> Result<(), AdbError> {
        let mut first = None;
        let mut completions = self.0.lock().await;
        for completion in completions.iter() {
            if let Err(e) = completion.close().await {
                first.get_or_insert(e);
            }
        }
        completions.clear();
        match first {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn close_finishes_a_cancelled_open_and_a_queued_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        let writer = AsyncFileWriter::new(path.clone());
        let completion = writer.completion();
        let (tx, rx) = tokio::sync::oneshot::channel();
        completion.0.lock().await.opening = Some(Box::pin(async move {
            rx.await.map_err(|e| AdbError::Io(e.to_string()))?;
            File::create(path).await.map_err(io_error)
        }));
        let mut start = Box::pin(writer.start());
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(start.as_mut().poll(cx).is_pending())).await);
        drop(start);
        tx.send(()).unwrap();
        // Model an outstanding Tokio blocking write, retained by the File after the awaiter disappears.
        {
            let mut state = completion.0.lock().await;
            state.finish_open().await.unwrap();
            let file = state.file.as_mut().unwrap();
            file.write_all(&vec![7; 65536]).await.unwrap();
        }
        drop(writer);
        completion.close().await.unwrap();
        assert_eq!(tokio::fs::read(dir.path().join("file")).await.unwrap(), vec![7; 65536]);
    }
}
