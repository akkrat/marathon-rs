//! Port of adam 0.5.8 `adam/src/test/kotlin/com/malinskiy/adam/request/pkg/*Test.kt` and `.../pkg/multi/*Test.kt`.
//! Copyright (C) 2021 Anton Malinskiy; Apache-2.0.
use super::*;
use crate::client::stub;
fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/adam/fixture/sample-fake.apk")
}
fn text(r: &impl Request) -> String {
    String::from_utf8(r.serialize()).unwrap()
}
fn create(features: Vec<Feature>) -> CreateIndividualPackageSessionRequest {
    let pkg = InstallationPackage::SingleFile(fixture());
    CreateIndividualPackageSessionRequest {
        pkg: pkg.clone(),
        pkg_list: vec![pkg],
        supported_features: features,
        reinstall: false,
        extra_args: vec![],
    }
}
async fn create_read(response: &'static str) -> Result<CreateSessionResponse, AdbError> {
    let s = stub::session(move |mut s| async move {
        s.expect_cmd("exec:cmd package install-create").await;
        s.accept().await;
        s.respond(response.as_bytes()).await;
    })
    .await;
    let r = s.client().execute(create(vec![Feature::Cmd]), None).await;
    s.finish().await;
    r
}
async fn commit_read(response: &'static str) -> Result<String, AdbError> {
    let s = stub::session(move |mut s| async move {
        s.expect_cmd("exec:cmd package install-commit parent-session-id").await;
        s.accept().await;
        s.respond(response.as_bytes()).await;
    })
    .await;
    let r = s
        .client()
        .execute(
            InstallCommitRequest::new("parent-session-id", vec![Feature::Cmd], false),
            None,
        )
        .await;
    s.finish().await;
    r
}
async fn write_read(response: &'static str) -> Result<String, AdbError> {
    let bytes = tokio::fs::read(fixture()).await.unwrap();
    let s = stub::session(move |mut s| async move {
        s.expect_cmd("exec:cmd package install-write -S 614 session-id sample-fake.apk -")
            .await;
        s.accept().await;
        s.expect_bytes(&bytes).await;
        s.respond(response.as_bytes()).await;
    })
    .await;
    let r = WriteIndividualPackageRequest::prepare(fixture(), vec![Feature::Cmd], "session-id".into())
        .await
        .unwrap();
    let completion = r.completion();
    let out = s.client().execute(r, None).await;
    completion.close().await.unwrap();
    s.finish().await;
    out
}
mod install_remote_package_request_test {
    use super::*;
    #[test]
    fn test_serialize() {
        assert_eq!(
            text(&InstallRemotePackageRequest::new(
                "/data/local/tmp/file.apk",
                false,
                vec![]
            )),
            "0032shell:pm install /data/local/tmp/file.apk;echo x$?"
        );
    }
    #[test]
    fn test_reinstall() {
        assert_eq!(
            text(&InstallRemotePackageRequest::new(
                "/data/local/tmp/file.apk",
                true,
                vec![]
            )),
            "0035shell:pm install -r /data/local/tmp/file.apk;echo x$?"
        );
    }
    #[test]
    fn test_multiple_reinstall() {
        assert_eq!(
            text(&InstallRemotePackageRequest::new(
                "/data/local/tmp/file.apk",
                true,
                vec!["-g -r".into()]
            )),
            "003Bshell:pm install -r -g -r /data/local/tmp/file.apk;echo x$?"
        );
    }
    #[test]
    fn test_validation() {
        for (ext, valid) in [("apk", true), ("apex", false), ("bin", false)] {
            assert_eq!(
                InstallRemotePackageRequest::new(format!("/data/local/tmp/file.{ext}"), true, vec![])
                    .validate()
                    .success,
                valid
            );
        }
    }
}
mod uninstall_remote_package_request_test {
    use super::*;
    #[test]
    fn test_serialize() {
        assert_eq!(
            text(&UninstallRemotePackageRequest::new("com.example", false)),
            "0027shell:pm uninstall com.example;echo x$?"
        );
    }
    #[test]
    fn test_serialize_with_remove_data_flag() {
        assert_eq!(
            text(&UninstallRemotePackageRequest::new("com.example", true)),
            "002Ashell:pm uninstall -k com.example;echo x$?"
        );
    }
}
mod create_individual_package_session_request_test {
    use super::*;
    #[test]
    fn serialize() {
        assert_eq!(text(&create(vec![Feature::Cmd])), "001Fexec:cmd package install-create");
    }
    #[test]
    fn serialize_abb() {
        assert_eq!(
            text(&create(vec![Feature::Cmd, Feature::AbbExec])),
            "001Fabb_exec:package\0install-create"
        );
    }
    #[test]
    fn serialize_no_features() {
        assert_eq!(text(&create(vec![])), "0016exec:pm install-create");
    }
    #[test]
    fn serialize_customized() {
        let mut r = create(vec![Feature::Cmd]);
        r.extra_args = vec!["-g".into()];
        r.reinstall = true;
        assert_eq!(text(&r), "0027exec:cmd package install-create '-g' -r");
    }
    #[test]
    fn serialized_apex() {
        let pkg = InstallationPackage::ApkSplit(vec![PathBuf::from("sample.apex")]);
        let r = CreateIndividualPackageSessionRequest {
            pkg: pkg.clone(),
            pkg_list: vec![pkg],
            supported_features: vec![Feature::Cmd],
            extra_args: vec!["-g".into()],
            reinstall: true,
        };
        assert_eq!(text(&r), "0037exec:cmd package install-create '-g' -r --staged --apex");
    }
    #[tokio::test]
    async fn test_read() {
        assert_eq!(
            create_read("Success [my-session-id]").await.unwrap(),
            CreateSessionResponse {
                id: "my-session-id".into(),
                output: "Success [my-session-id]".into()
            }
        );
    }
    #[tokio::test]
    async fn test_read_exception() {
        assert_eq!(
            create_read("Failure").await.unwrap_err(),
            AdbError::RequestRejected("Failed to create multi-package session: Failure".into())
        );
    }
    #[tokio::test]
    async fn test_read_no_session() {
        assert_eq!(
            create_read("Success no session returned").await.unwrap_err(),
            AdbError::RequestRejected("Failed to create multi-package session".into())
        );
    }
}
mod install_commit_request_test {
    use super::*;
    #[test]
    fn serialize() {
        assert_eq!(
            text(&InstallCommitRequest::new(
                "parent-session-id",
                vec![Feature::Cmd],
                false
            )),
            "0031exec:cmd package install-commit parent-session-id"
        );
    }
    #[test]
    fn serialize_abb() {
        assert_eq!(
            text(&InstallCommitRequest::new(
                "parent-session-id",
                vec![Feature::Cmd, Feature::AbbExec],
                false
            )),
            "0031abb_exec:package\0install-commit\0parent-session-id"
        );
    }
    #[test]
    fn serialize_abandon() {
        assert_eq!(
            text(&InstallCommitRequest::new(
                "parent-session-id",
                vec![Feature::Cmd, Feature::AbbExec],
                true
            )),
            "0032abb_exec:package\0install-abandon\0parent-session-id"
        );
    }
    #[test]
    fn test_serialize_no_features() {
        assert_eq!(
            text(&InstallCommitRequest::new("parent-session-id", vec![], false)),
            "0028exec:pm install-commit parent-session-id"
        );
    }
    #[tokio::test]
    async fn test_read() {
        assert_eq!(
            commit_read("Success [my-session-id]").await.unwrap(),
            "Success [my-session-id]"
        );
    }
    #[tokio::test]
    async fn test_read_exception() {
        assert_eq!(
            commit_read("Failure").await.unwrap_err(),
            AdbError::RequestRejected("Failed to finalize session parent-session-id: Failure".into())
        );
    }
}
mod write_individual_package_request_test {
    use super::*;
    async fn serialized(features: Vec<Feature>) -> String {
        let r = WriteIndividualPackageRequest::prepare(fixture(), features, "session-id".into())
            .await
            .unwrap();
        let out = text(&r);
        r.completion().close().await.unwrap();
        out
    }
    #[tokio::test]
    async fn serialize() {
        assert_eq!(
            serialized(vec![Feature::Cmd]).await,
            "0042exec:cmd package install-write -S 614 session-id sample-fake.apk -"
        );
    }
    #[tokio::test]
    async fn serialize_abb() {
        assert_eq!(
            serialized(vec![Feature::Cmd, Feature::AbbExec]).await,
            "0042abb_exec:package\0install-write\0-S\x00614\0session-id\0sample-fake.apk\0-"
        );
    }
    #[tokio::test]
    async fn serialize_no_features() {
        assert_eq!(
            serialized(vec![]).await,
            "0039exec:pm install-write -S 614 session-id sample-fake.apk -"
        );
    }
    #[tokio::test]
    async fn test_read() {
        assert_eq!(write_read("Success").await.unwrap(), "Success");
    }
    #[tokio::test]
    async fn test_read_exception() {
        assert!(matches!(write_read("Failure").await, Err(AdbError::RequestRejected(_))));
    }
}

#[tokio::test]
async fn streaming_payload_and_extra_args() {
    for abb in [false, true] {
        let bytes = tokio::fs::read(fixture()).await.unwrap();
        let s = stub::session(move |mut s| async move {
            s.expect_cmd(if abb {
                "abb_exec:package\0install\0--user\0a'b\0-r\0-S\x00614"
            } else {
                "exec:cmd package install '--user' 'a'\\''b' -r -S 614"
            })
            .await;
            s.accept().await;
            s.expect_bytes(&bytes).await;
            s.respond(b" \nSuccess details\r\n").await;
        })
        .await;
        let request = StreamingPackageInstallRequest::prepare(
            fixture(),
            if abb {
                vec![Feature::Cmd, Feature::AbbExec]
            } else {
                vec![Feature::Cmd]
            },
            true,
            vec!["--user".into(), "a'b".into()],
        )
        .await
        .unwrap();
        let completion = request.completion().unwrap();
        assert_eq!(
            s.client().execute(request, None).await.unwrap(),
            StreamingPackageInstallResult {
                output: "Success details".into(),
                success: true
            }
        );
        completion.close().await.unwrap();
        s.finish().await;
    }
}
#[tokio::test]
async fn validation_order_and_empty_split() {
    let p = PathBuf::from("/not-here.apex");
    let r = StreamingPackageInstallRequest::prepare(p, vec![], false, vec![])
        .await
        .unwrap();
    assert!(r.validate().message.unwrap().contains("doesn't exist"));
    let r = StreamingPackageInstallRequest::prepare(fixture(), vec![], false, vec![])
        .await
        .unwrap();
    assert_eq!(
        r.validate().message,
        Some(ValidationResponse::missing_either_feature(&[
            Feature::AbbExec,
            Feature::Cmd
        ]))
    );
    let r = InstallSplitPackageRequest::prepare(vec![], vec![], false, vec![])
        .await
        .unwrap();
    assert_eq!(
        r.validate().message,
        Some("At least one of the files has to be an APK file".into())
    );
}
#[tokio::test]
async fn commit_accepts_empty_and_create_contains_success() {
    assert_eq!(commit_read("").await.unwrap(), "");
    assert_eq!(create_read("prefix Success [abc]").await.unwrap().id, "abc");
}

fn oracle(case: &str) -> String {
    include_str!("../../../../../fixtures/oracle/adb-request-probes.jsonl")
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|v| v["case"] == case)
        .unwrap()["value"]
        .as_str()
        .unwrap()
        .into()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
#[tokio::test]
async fn serializers_match_jvm_bytes() {
    for features in [vec![], vec![Feature::Cmd], vec![Feature::Cmd, Feature::AbbExec]] {
        let label = features.iter().map(|f| f.name()).collect::<Vec<_>>().join(",");
        let mut c = create(features.clone());
        c.extra_args = vec!["-g".into()];
        c.reinstall = true;
        assert_eq!(hex(&c.serialize()), oracle(&format!("create:{label}")));
        let r = WriteIndividualPackageRequest::prepare(fixture(), features.clone(), "id".into())
            .await
            .unwrap();
        assert_eq!(hex(&r.serialize()), oracle(&format!("write:{label}")));
        r.completion().close().await.unwrap();
        assert_eq!(
            hex(&InstallCommitRequest::new("id", features.clone(), false).serialize()),
            oracle(&format!("commit:{label}"))
        );
        if !features.is_empty() {
            let r =
                StreamingPackageInstallRequest::prepare(fixture(), features, true, vec!["--user".into(), "a'b".into()])
                    .await
                    .unwrap();
            assert_eq!(hex(&r.serialize()), oracle(&format!("stream:{label}")));
            r.completion().unwrap().close().await.unwrap();
        }
    }
}
#[tokio::test]
async fn split_failures_abandon_in_order_and_rethrow_original() {
    for failure in ["create", "write", "commit", "abandon"] {
        let count = match failure {
            "create" => 1,
            "commit" => 4,
            _ => 3,
        };
        let server = stub::sessions(count, move |n, mut s| async move {
            let cmd = oracle(&format!("split-command:{failure}:{n}"));
            let bytes = cmd
                .as_bytes()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap())
                .collect::<Vec<_>>();
            let cmd = String::from_utf8(bytes).unwrap();
            s.expect_cmd(&cmd).await;
            s.accept().await;
            if cmd.contains("install-write") {
                s.expect_bytes(&tokio::fs::read(fixture()).await.unwrap()).await;
            }
            let response = if cmd.contains("install-create") {
                if failure == "create" {
                    "Failure create"
                } else {
                    "Success [S1]"
                }
            } else if cmd.contains("install-write") {
                if failure == "write" || failure == "abandon" {
                    "Failure write"
                } else {
                    "Success"
                }
            } else if cmd.contains("install-commit") {
                "Failure commit"
            } else if failure == "abandon" {
                "Failure abandon"
            } else {
                ""
            };
            s.respond(response.as_bytes()).await;
        })
        .await;
        let request = InstallSplitPackageRequest::prepare(vec![fixture()], vec![Feature::Cmd], true, vec![])
            .await
            .unwrap();
        let completion = request.completion();
        let error = request.execute(&server.client(), None).await.unwrap_err();
        completion.close().await.unwrap();
        let normalized = error.to_string().replace(&fixture().display().to_string(), "<apk>");
        assert_eq!(
            format!("{normalized}; cause=null"),
            oracle(&format!("split-result:{failure}"))
        );
        assert!(error.to_failure().cause.is_none());
        server.finish().await;
    }
}
