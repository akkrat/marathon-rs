//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/request/sync/{v1,v2,compat,base,model}/*.kt`.
//! Copyright (C) 2021 Anton Malinskiy; Apache-2.0.

use super::{
    AdbError, AndroidDebugBridgeClient, AsyncChannelRequest, ChannelStream, ComplexRequest, Feature, Request, Socket,
    SocketWriter, Target, ValidationResponse,
};
use super::{
    bytes,
    file_io::{AsyncFileReader, AsyncFileWriter, FileCompletion, io_error},
    request::{create_base_request, handshake},
};
use jcompat::jtime::{Instant, instant_of};
use std::{collections::VecDeque, path::PathBuf};

pub mod directory;
pub mod listing;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntryV1 {
    pub name: Option<String>,
    pub mode: u32,
    pub size: u32,
    pub mtime: Instant,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntryV2 {
    pub error: u32,
    pub dev: u64,
    pub ino: u64,
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub atime: Instant,
    pub mtime: Instant,
    pub ctime: Instant,
    pub name: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileEntry {
    V1(FileEntryV1),
    V2(FileEntryV2),
}
impl FileEntry {
    pub fn mode(&self) -> u32 {
        match self {
            Self::V1(f) => f.mode,
            Self::V2(f) => f.mode,
        }
    }
    pub fn size(&self) -> u64 {
        match self {
            Self::V1(f) => u64::from(f.size),
            Self::V2(f) => f.size,
        }
    }
    pub fn mtime(&self) -> Instant {
        match self {
            Self::V1(f) => f.mtime,
            Self::V2(f) => f.mtime,
        }
    }
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::V1(f) => f.name.as_deref(),
            Self::V2(f) => f.name.as_deref(),
        }
    }
    pub fn exists(&self) -> bool {
        !(self.mode() == 0 && self.size() == 0 && self.mtime().seconds() == 0)
    }
    pub fn is_directory(&self) -> bool {
        self.mode() & 0o040000 == 0o040000
    }
    pub fn is_regular_file(&self) -> bool {
        self.mode() & 0o100000 == 0o100000
    }
    pub fn is_block_device(&self) -> bool {
        self.mode() & 0o060000 == 0o060000
    }
    pub fn is_char_device(&self) -> bool {
        self.mode() & 0o020000 == 0o020000
    }
    pub fn is_link(&self) -> bool {
        self.mode() & 0o120000 == 0o120000
    }
}

pub(crate) fn unsupported() -> AdbError {
    AdbError::UnsupportedSyncProtocol("Unsupported sync: protocol response".into())
}
pub(crate) async fn record(socket: &mut Socket, n: usize) -> Result<Vec<u8>, AdbError> {
    if n > 65536 {
        return Err(unsupported());
    }
    let mut b = vec![0; n];
    if socket.read_fully(&mut b).await != n {
        return Err(unsupported());
    }
    Ok(b)
}
fn u32_at(b: &[u8], p: usize) -> u32 {
    u32::from_le_bytes([b[p], b[p + 1], b[p + 2], b[p + 3]])
}
fn long_at(b: &[u8], p: usize) -> i64 {
    bytes::to_long(&[
        b[p],
        b[p + 1],
        b[p + 2],
        b[p + 3],
        b[p + 4],
        b[p + 5],
        b[p + 6],
        b[p + 7],
    ])
}
fn time(seconds: i64) -> Result<Instant, AdbError> {
    instant_of(seconds, 0).ok_or_else(unsupported)
}
fn entry(b: &[u8], v2: bool, name: Option<String>) -> Result<FileEntry, AdbError> {
    Ok(if v2 {
        FileEntry::V2(FileEntryV2 {
            error: u32_at(b, 0),
            dev: long_at(b, 4) as u64,
            ino: long_at(b, 12) as u64,
            mode: u32_at(b, 20),
            nlink: u32_at(b, 24),
            uid: u32_at(b, 28),
            gid: u32_at(b, 32),
            size: long_at(b, 36) as u64,
            atime: time(long_at(b, 44))?,
            mtime: time(long_at(b, 52))?,
            ctime: time(long_at(b, 60))?,
            name,
        })
    } else {
        FileEntry::V1(FileEntryV1 {
            mode: u32_at(b, 0),
            size: u32_at(b, 4),
            mtime: time(i64::from(u32_at(b, 8) as i32))?,
            name,
        })
    })
}
fn path_validation(path: &str, bytes: bool) -> ValidationResponse {
    let length = if bytes { path.len() } else { path.encode_utf16().count() };
    if length > 1024 {
        invalid(ValidationResponse::path_should_not_be_long())
    } else {
        ValidationResponse::SUCCESS
    }
}
pub(crate) fn invalid(message: String) -> ValidationResponse {
    ValidationResponse::new(false, Some(message))
}
fn feature_validation(
    response: ValidationResponse,
    v2: bool,
    features: &[Feature],
    feature: Feature,
) -> ValidationResponse {
    if response.success && v2 && !features.contains(&feature) {
        invalid(ValidationResponse::missing_feature(feature))
    } else {
        response
    }
}

#[derive(Clone, Debug)]
pub struct StatFileRequest {
    pub remote_path: String,
    pub supported_features: Vec<Feature>,
    pub v2: bool,
}
impl StatFileRequest {
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            remote_path: path.into(),
            supported_features: vec![],
            v2: false,
        }
    }
    pub fn v2(path: impl Into<String>, features: Vec<Feature>) -> Self {
        Self {
            remote_path: path.into(),
            supported_features: features,
            v2: true,
        }
    }
}
impl Request for StatFileRequest {
    fn serialize(&self) -> Vec<u8> {
        create_base_request(&Target::NonSpecified, "sync:")
    }
    fn validate(&self) -> ValidationResponse {
        feature_validation(
            path_validation(&self.remote_path, false),
            self.v2,
            &self.supported_features,
            Feature::StatV2,
        )
    }
}
impl ComplexRequest for StatFileRequest {
    type Output = FileEntry;
    async fn read_element(&mut self, socket: &mut Socket) -> Result<FileEntry, AdbError> {
        let tag = if self.v2 { b"LST2" } else { b"STAT" };
        socket.write_sync_request(tag, &self.remote_path).await?;
        let b = record(socket, if self.v2 { 72 } else { 16 }).await?;
        if &b[..4] != tag {
            return Err(unsupported());
        }
        entry(&b[4..], self.v2, None)
    }
}

#[derive(Clone, Debug)]
pub struct ListFileRequest {
    pub remote_path: String,
    pub supported_features: Vec<Feature>,
    pub v2: bool,
}
impl ListFileRequest {
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            remote_path: path.into(),
            supported_features: vec![],
            v2: false,
        }
    }
    pub fn v2(path: impl Into<String>, features: Vec<Feature>) -> Self {
        Self {
            remote_path: path.into(),
            supported_features: features,
            v2: true,
        }
    }
}
impl Request for ListFileRequest {
    fn serialize(&self) -> Vec<u8> {
        create_base_request(&Target::NonSpecified, "sync:")
    }
    fn validate(&self) -> ValidationResponse {
        feature_validation(
            path_validation(&self.remote_path, false),
            self.v2,
            &self.supported_features,
            Feature::LsV2,
        )
    }
}
impl ComplexRequest for ListFileRequest {
    type Output = Vec<FileEntry>;
    async fn read_element(&mut self, socket: &mut Socket) -> Result<Self::Output, AdbError> {
        socket
            .write_sync_request(if self.v2 { b"LIS2" } else { b"LIST" }, &self.remote_path)
            .await?;
        let mut out = Vec::new();
        loop {
            let tag = record(socket, 4).await?;
            if tag != if self.v2 { b"DNT2" } else { b"DENT" } {
                return Ok(out);
            }
            let data = record(socket, if self.v2 { 72 } else { 16 }).await?;
            let n = u32_at(&data, data.len() - 4) as usize;
            let name = String::from_utf8_lossy(&record(socket, n).await?).into_owned();
            out.push(entry(&data, self.v2, Some(name))?);
        }
    }
}

pub struct PullFileRequest {
    pub remote_path: String,
    pub supported_features: Vec<Feature>,
    pub v2: bool,
    size: Option<i64>,
    writer: AsyncFileWriter,
    pub total_bytes: i64,
    pub current_position: i64,
}
impl PullFileRequest {
    pub fn new(path: impl Into<String>, local: PathBuf, size: Option<i64>) -> Self {
        Self {
            remote_path: path.into(),
            supported_features: vec![],
            v2: false,
            size,
            writer: AsyncFileWriter::new(local),
            total_bytes: -1,
            current_position: 0,
        }
    }
    pub fn v2(path: impl Into<String>, local: PathBuf, features: Vec<Feature>, size: Option<i64>) -> Self {
        Self {
            v2: true,
            supported_features: features,
            ..Self::new(path, local, size)
        }
    }
    pub fn completion(&self) -> FileCompletion {
        self.writer.completion()
    }
}
impl Request for PullFileRequest {
    fn serialize(&self) -> Vec<u8> {
        create_base_request(&Target::NonSpecified, "sync:")
    }
    fn validate(&self) -> ValidationResponse {
        feature_validation(
            path_validation(&self.remote_path, false),
            self.v2,
            &self.supported_features,
            Feature::SendrecvV2,
        )
    }
    async fn handshake(&mut self, socket: &mut Socket) -> Result<(), AdbError> {
        handshake(&self.serialize(), socket).await?;
        self.total_bytes = match self.size {
            Some(size) => size,
            None => StatFileRequest::new(&self.remote_path)
                .read_element(socket)
                .await?
                .size() as i64,
        };
        if self.total_bytes > 0 {
            self.writer.start().await?;
        }
        if self.v2 {
            socket.write_sync_v2_request(b"RCV2", &self.remote_path, 0, None).await
        } else {
            socket.write_sync_request(b"RECV", &self.remote_path).await
        }
    }
}
impl AsyncChannelRequest for PullFileRequest {
    type Item = f64;
    type Input = ();
    async fn read_element(&mut self, socket: &mut Socket, send: &mut VecDeque<f64>) -> Result<bool, AdbError> {
        let b = record(socket, 8).await?;
        match &b[..4] {
            b"DONE" => {
                self.writer.close().await?;
                Ok(true)
            }
            b"DATA" => {
                let data = record(socket, u32_at(&b, 4) as usize).await?;
                self.writer.write(&data).await?;
                self.current_position += data.len() as i64;
                send.push_back(self.current_position as f64 / self.total_bytes as f64);
                Ok(false)
            }
            b"FAIL" => Err(AdbError::PullFailed(format!(
                "Failed to pull file {}: {}",
                self.remote_path,
                String::from_utf8_lossy(&record(socket, u32_at(&b, 4) as usize).await?).into_owned()
            ))),
            tag => Err(AdbError::UnsupportedSyncProtocol(format!(
                "Unexpected header message {}",
                String::from_utf8_lossy(tag).into_owned()
            ))),
        }
    }
    async fn write_element(_: (), _: &SocketWriter) -> Result<(), AdbError> {
        Ok(())
    }
    async fn close(&mut self, _: &mut VecDeque<f64>) -> Result<(), AdbError> {
        self.writer.close().await
    }
}

pub struct PushFileRequest {
    pub remote_path: String,
    pub supported_features: Vec<Feature>,
    pub v2: bool,
    pub dry_run: bool,
    pub mode: String,
    reader: Option<AsyncFileReader>,
    validation: ValidationResponse,
    local: PathBuf,
    total_bytes: u64,
    current_position: u64,
}
impl PushFileRequest {
    pub async fn prepare(local: PathBuf, path: impl Into<String>, mode: impl Into<String>) -> Result<Self, AdbError> {
        let metadata = tokio::fs::metadata(&local).await;
        let validation = match &metadata {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                invalid(format!("Local file {} doesn't exist", local.display()))
            }
            Err(e) => return Err(AdbError::Io(e.to_string())),
            Ok(m) if !m.is_file() => invalid(format!("{} is not a file", local.display())),
            _ => ValidationResponse::SUCCESS,
        };
        let total_bytes = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
        let reader = if validation.success {
            Some(AsyncFileReader::prepare(&local).await?)
        } else {
            None
        };
        Ok(Self {
            remote_path: path.into(),
            supported_features: vec![],
            v2: false,
            dry_run: false,
            mode: mode.into(),
            reader,
            validation,
            local,
            total_bytes,
            current_position: 0,
        })
    }
    pub fn with_v2(mut self, features: Vec<Feature>, dry_run: bool) -> Self {
        self.v2 = true;
        self.supported_features = features;
        self.dry_run = dry_run;
        self
    }
    pub fn completion(&self) -> Option<FileCompletion> {
        self.reader.as_ref().map(AsyncFileReader::completion)
    }
    fn mode_value(&self) -> Result<u32, AdbError> {
        u32::from_str_radix(&self.mode, 8)
            .map(|m| m & 0o777)
            .map_err(|_| AdbError::Runtime(format!("Invalid radix 8 number: {}", self.mode)))
    }
}
pub(crate) fn epoch_seconds(t: std::time::SystemTime) -> i64 {
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}
impl Request for PushFileRequest {
    fn serialize(&self) -> Vec<u8> {
        create_base_request(&Target::NonSpecified, "sync:")
    }
    fn validate(&self) -> ValidationResponse {
        if !self.validation.success {
            return self.validation.clone();
        }
        let response = feature_validation(
            path_validation(&self.remote_path, true),
            self.v2,
            &self.supported_features,
            Feature::SendrecvV2,
        );
        if response.success
            && self.v2
            && self.dry_run
            && !self.supported_features.contains(&Feature::SendrecvV2DryRunSend)
        {
            invalid(ValidationResponse::missing_feature(Feature::SendrecvV2DryRunSend))
        } else {
            response
        }
    }
    async fn handshake(&mut self, socket: &mut Socket) -> Result<(), AdbError> {
        handshake(&self.serialize(), socket).await?;
        let mode = self.mode_value()?;
        if self.v2 {
            socket
                .write_sync_v2_request(
                    b"SND2",
                    &self.remote_path,
                    if self.dry_run { 0x80000000 } else { 0 },
                    Some(mode),
                )
                .await
        } else {
            socket
                .write_sync_request(b"SEND", &format!("{},{mode}", self.remote_path))
                .await
        }
    }
}
impl AsyncChannelRequest for PushFileRequest {
    type Item = f64;
    type Input = ();
    async fn read_element(&mut self, socket: &mut Socket, send: &mut VecDeque<f64>) -> Result<bool, AdbError> {
        let reader = self
            .reader
            .as_ref()
            .ok_or_else(|| AdbError::IllegalState("File reader unavailable".into()))?;
        let data = reader.read(65528).await?;
        if data.is_empty() {
            reader.close().await?;
            if self.current_position != self.total_bytes {
                return Err(AdbError::Io("Unexpected end of local file".into()));
            }
            let mut done = b"DONE".to_vec();
            // File.lastModified() is sampled at DONE and returns zero if metadata is unavailable.
            let mtime = tokio::fs::metadata(&self.local)
                .await
                .ok()
                .and_then(|m| m.modified().ok())
                .map(epoch_seconds)
                .unwrap_or(0);
            done.extend_from_slice(&(mtime as i32).to_be_bytes());
            socket.write(&done).await?;
            let response = socket.read_transport_response().await;
            if !response.okay {
                return Err(AdbError::PushFailed(format!(
                    "adb didn't acknowledge the file transfer: {}",
                    response.message.unwrap_or_default()
                )));
            }
            send.push_back(1.0);
            return Ok(true);
        }
        let mut packet = b"DATA".to_vec();
        packet.extend_from_slice(&(data.len() as u32).to_le_bytes());
        packet.extend_from_slice(&data);
        socket.write(&packet).await?;
        self.current_position += data.len() as u64;
        send.push_back(self.current_position as f64 / self.total_bytes as f64);
        Ok(false)
    }
    async fn write_element(_: (), _: &SocketWriter) -> Result<(), AdbError> {
        Ok(())
    }
    async fn close(&mut self, _: &mut VecDeque<f64>) -> Result<(), AdbError> {
        if let Some(r) = &self.reader {
            r.close().await?;
        }
        Ok(())
    }
}

pub struct CompatStatFileRequest(pub StatFileRequest);
impl CompatStatFileRequest {
    pub fn new(path: impl Into<String>, features: Vec<Feature>) -> Self {
        Self(if features.contains(&Feature::StatV2) {
            StatFileRequest::v2(path, features)
        } else {
            StatFileRequest::new(path)
        })
    }
    pub async fn execute(self, client: &AndroidDebugBridgeClient, serial: Option<&str>) -> Result<FileEntry, AdbError> {
        client.execute(self.0, serial).await
    }
}
pub struct CompatListFileRequest(pub ListFileRequest);
impl CompatListFileRequest {
    pub fn new(path: impl Into<String>, features: Vec<Feature>) -> Self {
        Self(if features.contains(&Feature::LsV2) {
            ListFileRequest::v2(path, features)
        } else {
            ListFileRequest::new(path)
        })
    }
    pub async fn execute(
        self,
        client: &AndroidDebugBridgeClient,
        serial: Option<&str>,
    ) -> Result<Vec<FileEntry>, AdbError> {
        client.execute(self.0, serial).await
    }
}
pub struct CompatPullFileRequest(pub PullFileRequest);
impl CompatPullFileRequest {
    pub fn new(path: impl Into<String>, local: PathBuf, features: Vec<Feature>, size: Option<i64>) -> Self {
        Self(if features.contains(&Feature::SendrecvV2) {
            PullFileRequest::v2(path, local, features, size)
        } else {
            PullFileRequest::new(path, local, size)
        })
    }
    pub fn execute(
        self,
        client: &AndroidDebugBridgeClient,
        serial: Option<&str>,
    ) -> Result<ChannelStream<PullFileRequest>, AdbError> {
        client.execute_channel(self.0, serial)
    }
}
pub struct CompatPushFileRequest(pub PushFileRequest);
impl CompatPushFileRequest {
    pub async fn prepare(
        local: PathBuf,
        path: impl Into<String>,
        features: Vec<Feature>,
        mode: impl Into<String>,
    ) -> Result<Self, AdbError> {
        let mut request = PushFileRequest::prepare(local, path, mode).await?;
        if features.contains(&Feature::SendrecvV2) {
            request = request.with_v2(features, false);
        }
        Ok(Self(request))
    }
    pub fn execute(
        self,
        client: &AndroidDebugBridgeClient,
        serial: Option<&str>,
    ) -> Result<ChannelStream<PushFileRequest>, AdbError> {
        client.execute_channel(self.0, serial)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompressionType {
    None,
    Brotli,
    Lz4,
    Zstd,
}
impl CompressionType {
    pub fn to_flag(self) -> u32 {
        match self {
            Self::None => 0,
            Self::Brotli => 1,
            Self::Lz4 => 2,
            Self::Zstd => 4,
        }
    }
}

#[cfg(test)]
mod tests;
