//! Port of adam/src/main/kotlin/com/malinskiy/adam/request/prop/GetPropRequest.kt
//! and request/logcat/ChanneledLogcatRequest.kt (adam 0.5.8).
//! Copyright (C) 2021 Anton Malinskiy; Apache-2.0.
use jcompat::jstring;
use marathon_adb::client::{
    AdbError, ComplexRequest, Request, Socket, Target,
    shell::v1::{ChanneledShellCommandRequest, ShellCommandRequest},
};
use std::collections::HashMap;
#[derive(Default)]
pub struct GetPropRequest;
impl Request for GetPropRequest {
    fn serialize(&self) -> Vec<u8> {
        ShellCommandRequest::new("getprop").serialize()
    }
}
impl ComplexRequest for GetPropRequest {
    type Output = HashMap<String, String>;
    async fn read_element(&mut self, socket: &mut Socket) -> Result<Self::Output, AdbError> {
        let response = ShellCommandRequest::new("getprop").read_element(socket).await?;
        parse_props(&response.output().as_str().to_owned())
    }
}
pub fn parse_props(text: &str) -> Result<HashMap<String, String>, AdbError> {
    let mut props = HashMap::new();
    for line in jstring::lines(text) {
        if line.is_empty() || line.encode_utf16().collect::<Vec<_>>()[0] == 35 {
            continue;
        }
        let opens: Vec<_> = line
            .encode_utf16()
            .collect::<Vec<_>>()
            .iter()
            .enumerate()
            .filter_map(|(i, u)| (*u == 91).then_some(i))
            .collect();
        let closes: Vec<_> = line
            .encode_utf16()
            .collect::<Vec<_>>()
            .iter()
            .enumerate()
            .filter_map(|(i, u)| (*u == 93).then_some(i))
            .collect();
        if opens.len() != 2 || closes.len() != 2 {
            continue;
        }
        let slice = |index: usize| {
            line.encode_utf16()
                .collect::<Vec<_>>()
                .get(opens[index] + 1..closes[index])
                .map(|s| String::from_utf16_lossy(s))
                .ok_or_else(|| {
                    AdbError::StringIndexOutOfBounds(format!(
                        "Range [{}, {}) out of bounds for length {}",
                        opens[index] + 1,
                        closes[index],
                        line.encode_utf16().count()
                    ))
                })
        };
        props.insert(slice(0)?.to_owned(), slice(1)?);
    }
    Ok(props)
}
pub fn logcat_request(modes: &[&str]) -> ChanneledShellCommandRequest {
    ChanneledShellCommandRequest::with(
        format!(
            "logcat {}",
            modes.iter().map(|m| format!("-v {m}")).collect::<Vec<_>>().join(" ")
        ),
        Target::NonSpecified,
        Some(i64::MAX),
    )
}

/// D11/P4: retain a partial line, including a CR whose LF may arrive next.
#[derive(Default)]
pub struct LogcatLines {
    pending: String,
}
impl LogcatLines {
    pub fn push(&mut self, chunk: &str) -> Vec<String> {
        self.pending.push_str(chunk);
        let mut lines = Vec::new();
        while let Some(end) = self.pending.find(['\r', '\n']) {
            let cr = self.pending.as_bytes()[end] == b'\r';
            if cr && end + 1 == self.pending.len() {
                break;
            }
            let width = if cr && self.pending.as_bytes().get(end + 1) == Some(&b'\n') {
                2
            } else {
                1
            };
            lines.push(self.pending[..end].into());
            self.pending.drain(..end + width);
        }
        lines
    }
    pub fn finish(&mut self) -> Vec<String> {
        let text = std::mem::take(&mut self.pending);
        if text.is_empty() {
            Vec::new()
        } else {
            jstring::lines(text.as_str())
        }
    }
}
