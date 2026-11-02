//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/request/abb/AbbExecRequest.kt`.
//! adam portions: Copyright (C) 2019-2021 Anton Malinskiy; Apache-2.0.

use super::consts::POOLED_DEFAULT_BUFFER_SIZE;
use super::error::AdbError;
use super::feature::Feature;
use super::request::{ComplexRequest, Request, create_base_request};
use super::target::Target;
use super::transform::{ResponseTransformer, StringResponseTransformer};
use super::transport::Socket;
use super::validation::ValidationResponse;

/// `AbbExecRequest.DELIMITER`: abb_exec separates its arguments with NUL.
pub const DELIMITER: char = '\0';

/// `abb_exec:<args joined by NUL>`: a binder call over a raw pipe; the reply is read to the end and
/// trimmed. Needs `ABB_EXEC`.
#[derive(Debug)]
pub struct AbbExecRequest {
    args: Vec<String>,
    supported_features: Vec<Feature>,
    transformer: StringResponseTransformer,
}

impl AbbExecRequest {
    pub fn new(args: Vec<String>, supported_features: Vec<Feature>) -> Self {
        Self {
            args,
            supported_features,
            transformer: StringResponseTransformer::default(),
        }
    }
}

/// The `abb_exec:` service string, which 6.2's streaming install also sends.
pub fn serialize_abb_exec(args: &[String]) -> Vec<u8> {
    create_base_request(
        &Target::NonSpecified,
        &format!("abb_exec:{}", args.join(&DELIMITER.to_string())),
    )
}

impl Request for AbbExecRequest {
    fn serialize(&self) -> Vec<u8> {
        serialize_abb_exec(&self.args)
    }

    fn validate(&self) -> ValidationResponse {
        if !self.supported_features.contains(&Feature::AbbExec) {
            return ValidationResponse::new(false, Some(ValidationResponse::missing_feature(Feature::AbbExec)));
        }
        ValidationResponse::SUCCESS
    }
}

impl ComplexRequest for AbbExecRequest {
    type Output = String;

    async fn read_element(&mut self, socket: &mut Socket) -> Result<String, AdbError> {
        socket
            .copy_to(&mut self.transformer, POOLED_DEFAULT_BUFFER_SIZE, None)
            .await;
        self.transformer.transform()
    }
}

#[cfg(test)]
mod abb_exec_request_test {
    use super::*;
    use crate::client::stub;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn test_serialize() {
        let request = AbbExecRequest::new(strings(&["cmd", "package", "install"]), vec![Feature::AbbExec]);
        assert_eq!(
            String::from_utf8(request.serialize()).unwrap(),
            "001Cabb_exec:cmd\u{0}package\u{0}install"
        );
    }

    #[test]
    fn test_validation() {
        assert!(AbbExecRequest::new(vec![], vec![Feature::AbbExec]).validate().success);
    }

    #[test]
    fn test_validation_failure() {
        assert!(!AbbExecRequest::new(vec![], vec![]).validate().success);
    }

    #[tokio::test]
    async fn test_dummy() {
        let socket = stub::StubSocket::new(b"cafebabe").await;
        let mut request = AbbExecRequest::new(vec![], vec![]);
        let output = request.read_element(&mut socket.connect().await).await.unwrap();
        assert_eq!(output, "cafebabe");
    }
}
