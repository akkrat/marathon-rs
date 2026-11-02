//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/request/sync/{PushRequest,BFFSearch}.kt`
//! and `.../extension/List.kt`.
//! Copyright (C) 2021 Anton Malinskiy; Apache-2.0.

use super::*;
use crate::client::shell::v1::ShellCommandRequest;

pub fn bash_escape(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub struct PushRequest {
    pub source: PathBuf,
    pub destination: String,
    pub supported_features: Vec<Feature>,
    pub mode: String,
    completion: super::super::file_io::FileCompletions,
}
impl PushRequest {
    pub fn new(source: PathBuf, destination: String, features: Vec<Feature>) -> Self {
        Self {
            source,
            destination,
            supported_features: features,
            mode: "0777".into(),
            completion: Default::default(),
        }
    }
    pub fn completion(&self) -> super::super::file_io::FileCompletions {
        self.completion.clone()
    }
    pub async fn execute(self, client: &AndroidDebugBridgeClient, serial: Option<&str>) -> Result<bool, AdbError> {
        let target = CompatStatFileRequest::new(&self.destination, self.supported_features.clone())
            .execute(client, serial)
            .await?;
        let metadata = match tokio::fs::metadata(&self.source).await {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(AdbError::PushFailed(format!(
                    "Source {} doesn't exist",
                    self.source.display()
                )));
            }
            Err(e) => return Err(io_error(e)),
        };
        let name = self.source.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
        let destination = if target.is_directory() {
            format!(
                "{}/{}",
                self.destination.strip_suffix('/').unwrap_or(&self.destination),
                name
            )
        } else {
            self.destination.clone()
        };
        if metadata.is_file() {
            if !target.is_directory() && !target.is_regular_file() && target.exists() {
                return Err(self.bad_target(target.mode()));
            }
            return self.push_file(client, serial, self.source.clone(), destination).await;
        }
        if !metadata.is_dir() {
            return Err(AdbError::PushFailed(format!(
                "Source {} is not a directory or a file",
                self.source.display()
            )));
        }
        // adam tests the directory bit first, so a mode with both bits (e.g. a socket) is a folder.
        if !target.is_directory() {
            if target.is_regular_file() {
                return Err(AdbError::PushFailed(format!(
                    "Can't push folder {}: target {} is a file",
                    self.source.display(),
                    self.destination
                )));
            }
            if target.exists() {
                return Err(self.bad_target(target.mode()));
            }
        }
        let destination = destination.strip_suffix('/').unwrap_or(&destination);
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        let mut queue = VecDeque::from([self.source.clone()]);
        while let Some(dir) = queue.pop_front() {
            let mut entries = tokio::fs::read_dir(&dir).await.map_err(io_error)?;
            while let Some(entry) = entries.next_entry().await.map_err(io_error)? {
                let path = entry.path();
                let relative = path
                    .strip_prefix(&self.source)
                    .map_err(|e| AdbError::Io(e.to_string()))?;
                let remote = format!("{destination}/{}", relative.to_string_lossy());
                let metadata = tokio::fs::metadata(&path).await.map_err(io_error)?;
                if metadata.is_dir() {
                    dirs.push(remote);
                    queue.push_back(path);
                } else if metadata.is_file() {
                    files.push((path, remote));
                }
            }
        }
        client
            .execute(
                ShellCommandRequest::new(format!("mkdir -p {}", bash_escape(destination))),
                serial,
            )
            .await?;
        let mut command = "mkdir".to_string();
        for dir in dirs {
            let arg = format!(" {}", bash_escape(&dir));
            if command.encode_utf16().count() + arg.encode_utf16().count() > 4096 {
                client.execute(ShellCommandRequest::new(&command), serial).await?;
                command = "mkdir".into();
            }
            command.push_str(&arg);
        }
        // Kotlin sends this even when no subdirectories were found.
        client.execute(ShellCommandRequest::new(command), serial).await?;
        for (local, remote) in files {
            if !self.push_file(client, serial, local, remote).await? {
                return Ok(false);
            }
        }
        Ok(true)
    }
    fn bad_target(&self, mode: u32) -> AdbError {
        AdbError::PushFailed(format!(
            "Target {} exists and is not a directory or a file: mode={mode:o}",
            self.destination
        ))
    }
    async fn push_file(
        &self,
        client: &AndroidDebugBridgeClient,
        serial: Option<&str>,
        local: PathBuf,
        remote: String,
    ) -> Result<bool, AdbError> {
        let request =
            CompatPushFileRequest::prepare(local, remote, self.supported_features.clone(), &self.mode).await?;
        let completion = request.0.completion();
        if let Some(c) = &completion {
            self.completion.register(c.clone()).await;
        }
        let result = async {
            let mut stream = request.execute(client, serial)?;
            let mut progress = 0.0;
            while let Some(update) = stream.next().await {
                progress = update?;
            }
            Ok(progress == 1.0)
        }
        .await;
        if let Some(completion) = completion {
            completion.close().await?;
        }
        result
    }
}
