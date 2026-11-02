//! Port of adam 0.5.8 `adam/src/test/kotlin/com/malinskiy/adam/request/reverse/{ReversePortForwardRequestTest,RemoveReversePortForwardRequestTest}.kt`.
//! Copyright (C) 2019 Anton Malinskiy; Apache-2.0.
use super::*;
use crate::client::stub;
fn request(mode: PortForwardingMode) -> ReversePortForwardRequest {
    ReversePortForwardRequest {
        local: RemotePortSpec::Tcp(80),
        remote: LocalPortSpec::Tcp(80),
        mode,
    }
}
mod reverse_port_forward_request_test {
    use super::*;
    #[test]
    fn test_serialize_default() {
        assert_eq!(
            request(PortForwardingMode::Default).serialize(),
            b"001Dreverse:forward:tcp:80;tcp:80"
        );
    }
    #[test]
    fn test_serialize_no_default() {
        assert_eq!(
            request(PortForwardingMode::NoRebind).serialize(),
            b"0026reverse:forward:norebind:tcp:80;tcp:80"
        );
    }
    #[tokio::test]
    async fn test_read() {
        let s = stub::session(|mut s| async move {
            s.expect_cmd("reverse:forward:tcp:80;tcp:80").await;
            s.accept().await;
            s.respond(b"OKAY00045000").await;
        })
        .await;
        assert_eq!(
            s.client()
                .execute(request(PortForwardingMode::Default), None)
                .await
                .unwrap(),
            Some(5000)
        );
        s.finish().await;
    }
    #[tokio::test]
    async fn test_read_failure() {
        let s = stub::session(|mut s| async move {
            s.expect_cmd("reverse:forward:tcp:80;tcp:80").await;
            s.accept().await;
            s.reject("CAFEBABE").await;
        })
        .await;
        assert_eq!(
            s.client()
                .execute(request(PortForwardingMode::Default), None)
                .await
                .unwrap_err(),
            AdbError::RequestRejected("Can't establish port forwarding: CAFEBABE".into())
        );
        s.finish().await;
    }
}
mod remove_reverse_port_forward_request_test {
    use super::*;
    #[test]
    fn test_serialize_default() {
        assert_eq!(
            RemoveReversePortForwardRequest {
                local: RemotePortSpec::Tcp(80)
            }
            .serialize(),
            b"001Areverse:killforward:tcp:80"
        );
    }
    #[tokio::test]
    async fn test_dummy() {
        let s = stub::session(|mut s| async move {
            s.expect_cmd("reverse:killforward:tcp:80").await;
            s.accept().await;
            s.respond(b"unused").await;
        })
        .await;
        s.client()
            .execute(
                RemoveReversePortForwardRequest {
                    local: RemotePortSpec::Tcp(80),
                },
                None,
            )
            .await
            .unwrap();
        s.finish().await;
    }
}
#[tokio::test]
async fn missing_or_noninteger_port_is_none() {
    for reply in [b"OKAY".as_slice(), b"OKAY0003bad", b"OKAY000A2147483648"] {
        let s = stub::session(move |mut s| async move {
            s.expect_cmd("reverse:forward:tcp:80;tcp:80").await;
            s.accept().await;
            s.respond(reply).await;
        })
        .await;
        assert_eq!(
            s.client()
                .execute(request(PortForwardingMode::Default), None)
                .await
                .unwrap(),
            None
        );
        s.finish().await;
    }
}
#[test]
fn spec_parsers_keep_only_second_segment() {
    assert_eq!(
        LocalPortSpec::parse("local:a:b").unwrap(),
        LocalPortSpec::UnixSocket("a".into())
    );
    for s in [
        "tcp:12",
        "localabstract:a",
        "localreserved:a",
        "localfilesystem:a",
        "dev:a",
        "jdwp:12",
    ] {
        assert_eq!(RemotePortSpec::parse(s).unwrap().to_spec(), s);
    }
    assert!(matches!(
        RemotePortSpec::parse("unknown:a"),
        Err(AdbError::UnsupportedForwardingSpec(_))
    ));
}

#[test]
fn invalid_numeric_specs_are_number_format_exceptions() {
    for value in ["bad", "", "2147483648", "-2147483649", " 42", "42 ", "+", "--1"] {
        for result in [
            LocalPortSpec::parse(&format!("tcp:{value}")).map(|_| ()),
            RemotePortSpec::parse(&format!("tcp:{value}")).map(|_| ()),
            RemotePortSpec::parse(&format!("jdwp:{value}")).map(|_| ()),
        ] {
            let error = result.unwrap_err();
            assert_eq!(error.java_class(), "java.lang.NumberFormatException");
            assert_eq!(error.message(), Some(format!("For input string: \"{value}\"").as_str()));
        }
    }
    assert_eq!(LocalPortSpec::parse("tcp:+42").unwrap(), LocalPortSpec::Tcp(42));
}
