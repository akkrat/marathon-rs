//! Key-file formats for `PublicKey` authentication (T13): every committed test-only key loads, or fails, as
//! sshj 0.39.0 does in the `keys` oracle probe (`fixtures/oracle/ssh/key_formats.jsonl`).
mod support;
use marathon_apple::cmd::{
    CommandExecutor,
    remote::ssh::sshj::{
        SshAuthentication, SshFailure,
        known_hosts::{key_type_of, md5_fingerprint},
    },
};
use std::path::{Path, PathBuf};
use support::ssh;

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
fn oracle() -> Vec<serde_json::Value> {
    std::fs::read_to_string(repository().join("fixtures/oracle/ssh/key_formats.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn chain(failure: &SshFailure) -> Vec<(String, Option<String>)> {
    std::iter::successors(Some(failure), |f| f.cause.as_deref())
        .map(|f| (f.class.to_owned(), f.message()))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn key_files_load_or_fail_like_sshj() {
    let rows = oracle();
    // Unencrypted OpenSSH, PEM, PKCS#8 and PPK v2/v3 load; encrypted keys without a passphrase fail.
    assert_eq!(rows.iter().filter(|r| r.get("fingerprint").is_some()).count(), 10);
    let scratch = tempfile::tempdir().unwrap();
    let mut authorized = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let Some(fingerprint) = row.get("fingerprint") else {
            continue;
        };
        let text = std::fs::read_to_string(repository().join(row["case"].as_str().unwrap())).unwrap();
        let key = russh::keys::decode_secret_key(&text, None).unwrap();
        assert_eq!(
            md5_fingerprint(key.public_key()),
            fingerprint.as_str().unwrap(),
            "{}",
            row["case"]
        );
        assert_eq!(key_type_of(key.public_key()), row["type"].as_str(), "{}", row["case"]);
        let path = scratch.path().join(format!("{i}.pub"));
        std::fs::write(&path, key.public_key().to_openssh().unwrap()).unwrap();
        authorized.push(path);
    }
    let fake = ssh::fake_with(ssh::suite_script(), 4, |config| {
        config.auth.authorized_keys.extend(authorized)
    })
    .await;
    let (logging, _) = ssh::logging(false);
    let factory = ssh::factory(&logging);
    for row in &rows {
        let auth = SshAuthentication::PublicKey {
            username: "fixture".into(),
            key: repository().join(row["case"].as_str().unwrap()),
        };
        let connected = factory.connect("127.0.0.1", fake.port(), auth, None, false).await;
        match row.get("public_error") {
            None => {
                let executor = connected.unwrap();
                assert_eq!(
                    ssh::run(&executor, &["echo", "hello"], 5000).await.unwrap().stdout,
                    ["hello"]
                );
                executor.close().await.unwrap();
            }
            Some(expected) => {
                // authPublickey asks the provider for the public key first: UserAuthException("Exhausted…")
                // over the UserAuthException that wraps the provider failure.
                let error = connected.err().unwrap();
                let expected: Vec<_> = expected
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|e| {
                        (
                            e["class"].as_str().unwrap().to_owned(),
                            e["message"].as_str().map(Into::into),
                        )
                    })
                    .collect();
                assert_eq!(chain(&error)[2..], expected[..], "{}", row["case"]);
            }
        }
    }
}
