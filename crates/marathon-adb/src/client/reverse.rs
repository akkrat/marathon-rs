//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/request/reverse/{ReversePortForwardRequest,
//! RemoveReversePortForwardRequest}.kt` and `.../forwarding/{LocalPortSpec,RemotePortSpec,PortForwardRequest}.kt`
//! (only PortForwardingMode from PortForwardRequest).
//! Copyright (C) 2019 Anton Malinskiy; Apache-2.0.

use super::{AdbError, ComplexRequest, Request, Socket, Target, request::create_base_request};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalPortSpec {
    Tcp(i32),
    UnixSocket(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemotePortSpec {
    Tcp(i32),
    Abstract(String),
    Reserved(String),
    Filesystem(String),
    Dev(String),
    Jdwp(i32),
}
fn parts(value: &str) -> Result<(&str, &str), AdbError> {
    let mut split = value.split(':');
    let kind = split.next().unwrap_or("");
    if !matches!(
        kind,
        "tcp" | "local" | "localabstract" | "localreserved" | "localfilesystem" | "dev" | "jdwp"
    ) {
        return Err(unknown(kind));
    }
    let arg = split
        .next()
        .ok_or_else(|| AdbError::MalformedResponse("Index 1 out of bounds for length 1".into()))?;
    Ok((kind, arg))
}
fn number(value: &str) -> Result<i32, AdbError> {
    jcompat::jnum::parse_int(value).ok_or_else(|| AdbError::NumberFormat(format!("For input string: \"{value}\"")))
}
fn unknown(kind: &str) -> AdbError {
    AdbError::UnsupportedForwardingSpec(format!("Unknown type {kind} for forwarding spec"))
}
impl LocalPortSpec {
    pub fn parse(value: &str) -> Result<Self, AdbError> {
        let kind = value.split(':').next().unwrap_or("");
        if !matches!(kind, "tcp" | "local") {
            return Err(unknown(kind));
        }
        let (kind, arg) = parts(value)?;
        match kind {
            "tcp" => Ok(Self::Tcp(number(arg)?)),
            "local" => Ok(Self::UnixSocket(arg.into())),
            _ => Err(unknown(kind)),
        }
    }
    pub fn to_spec(&self) -> String {
        match self {
            Self::Tcp(p) => format!("tcp:{p}"),
            Self::UnixSocket(p) => format!("local:{p}"),
        }
    }
}
impl RemotePortSpec {
    pub fn parse(value: &str) -> Result<Self, AdbError> {
        let kind = value.split(':').next().unwrap_or("");
        if !matches!(
            kind,
            "tcp" | "localabstract" | "localreserved" | "localfilesystem" | "dev" | "jdwp"
        ) {
            return Err(unknown(kind));
        }
        let (kind, arg) = parts(value)?;
        Ok(match kind {
            "tcp" => Self::Tcp(number(arg)?),
            "localabstract" => Self::Abstract(arg.into()),
            "localreserved" => Self::Reserved(arg.into()),
            "localfilesystem" => Self::Filesystem(arg.into()),
            "dev" => Self::Dev(arg.into()),
            "jdwp" => Self::Jdwp(number(arg)?),
            _ => return Err(unknown(kind)),
        })
    }
    pub fn to_spec(&self) -> String {
        match self {
            Self::Tcp(p) => format!("tcp:{p}"),
            Self::Abstract(p) => format!("localabstract:{p}"),
            Self::Reserved(p) => format!("localreserved:{p}"),
            Self::Filesystem(p) => format!("localfilesystem:{p}"),
            Self::Dev(p) => format!("dev:{p}"),
            Self::Jdwp(p) => format!("jdwp:{p}"),
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PortForwardingMode {
    #[default]
    Default,
    NoRebind,
}
pub struct ReversePortForwardRequest {
    pub local: RemotePortSpec,
    pub remote: LocalPortSpec,
    pub mode: PortForwardingMode,
}
impl Request for ReversePortForwardRequest {
    fn serialize(&self) -> Vec<u8> {
        create_base_request(
            &Target::NonSpecified,
            &format!(
                "reverse:forward{}:{};{}",
                if self.mode == PortForwardingMode::NoRebind {
                    ":norebind"
                } else {
                    ""
                },
                self.local.to_spec(),
                self.remote.to_spec()
            ),
        )
    }
}
impl ComplexRequest for ReversePortForwardRequest {
    type Output = Option<i32>;
    async fn read_element(&mut self, s: &mut Socket) -> Result<Self::Output, AdbError> {
        let response = s.read_transport_response().await;
        if !response.okay {
            return Err(AdbError::RequestRejected(format!(
                "Can't establish port forwarding: {}",
                response.message.unwrap_or_default()
            )));
        }
        Ok(jcompat::jnum::parse_int(&s.read_optional_protocol_string().await))
    }
}
pub struct RemoveReversePortForwardRequest {
    pub local: RemotePortSpec,
}
impl Request for RemoveReversePortForwardRequest {
    fn serialize(&self) -> Vec<u8> {
        create_base_request(
            &Target::NonSpecified,
            &format!("reverse:killforward:{}", self.local.to_spec()),
        )
    }
}
impl ComplexRequest for RemoveReversePortForwardRequest {
    type Output = ();
    async fn read_element(&mut self, s: &mut Socket) -> Result<(), AdbError> {
        let _ = s.read_status().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
