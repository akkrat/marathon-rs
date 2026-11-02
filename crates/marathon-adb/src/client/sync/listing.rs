//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/request/file/ListFilesRequest.kt`.
//! Copyright (C) 2020 Anton Malinskiy; Apache-2.0.

use super::*;
use crate::client::{
    request::read_synchronous,
    shell::v1::{ShellResultResponseTransformer, serialize_sync_shell},
    transform::kotlin_trim,
};
use jcompat::JavaRegex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AndroidFileType {
    RegularFile,
    Directory,
    BlockSpecialFile,
    CharacterSpecialFile,
    SymbolicLink,
    SocketLink,
    Fifo,
    Other,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AndroidFile {
    pub permissions: String,
    pub owner: String,
    pub group: String,
    pub date: String,
    pub time: String,
    pub name: String,
    pub directory: String,
    pub size: i64,
    pub file_type: AndroidFileType,
    pub link: Option<String>,
}

pub struct ListFilesRequest {
    pub directory: String,
}
impl ListFilesRequest {
    pub fn new(directory: impl Into<String>) -> Self {
        Self {
            directory: directory.into(),
        }
    }
    pub fn parse(&self, text: &str) -> Result<Vec<AndroidFile>, AdbError> {
        let regex = JavaRegex::compile(r"^([bcdlsp-][-r][-w][-xsS][-r][-w][-xsS][-r][-w][-xstST])\s+(?:\d+\s+)?(\S+)\s+(\S+)\s+([\d\s,]*)\s+(\d{4}-\d\d-\d\d)\s+(\d\d:\d\d)\s+(.*)$","ListFilesRequest")
            .map_err(|e| AdbError::IllegalState(e.to_string()))?;
        let mut out = Vec::new();
        for line in text.split(['\n', '\r']).filter(|l| !l.is_empty()) {
            let Some(c) = regex.find(line) else {
                continue;
            };
            let file_type = match &c[1][..1] {
                "-" => AndroidFileType::RegularFile,
                "d" => AndroidFileType::Directory,
                "b" => AndroidFileType::BlockSpecialFile,
                "c" => AndroidFileType::CharacterSpecialFile,
                "l" => AndroidFileType::SymbolicLink,
                "s" => AndroidFileType::SocketLink,
                "p" => AndroidFileType::Fifo,
                _ => AndroidFileType::Other,
            };
            let mut name = c[7].to_string();
            let mut link = None;
            if file_type == AndroidFileType::SymbolicLink {
                let split: Vec<_> = name.split("->").collect();
                if split.len() != 2 {
                    return Err(AdbError::Runtime(format!(
                        "Unable to parse the symbolic file entry {name}"
                    )));
                }
                link = Some(kotlin_trim(split[1]).into());
                name = kotlin_trim(split[0]).into();
            }
            out.push(AndroidFile {
                permissions: c[1].into(),
                owner: c[2].into(),
                group: c[3].into(),
                size: c[4].parse().unwrap_or(0),
                date: c[5].into(),
                time: c[6].into(),
                name,
                directory: self.directory.clone(),
                file_type,
                link,
            });
        }
        Ok(out)
    }
}
impl Request for ListFilesRequest {
    fn serialize(&self) -> Vec<u8> {
        serialize_sync_shell(&Target::NonSpecified, &format!("ls -l {}", self.directory))
    }
}
impl ComplexRequest for ListFilesRequest {
    type Output = Vec<AndroidFile>;
    async fn read_element(&mut self, socket: &mut Socket) -> Result<Self::Output, AdbError> {
        let response = read_synchronous(socket, &mut ShellResultResponseTransformer::default()).await?;
        self.parse(&response.output())
    }
}
