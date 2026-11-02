//! sshj 0.39.0 `OpenSSHKnownHosts` (R3, T17) replayed from the JVM oracle table
//! `fixtures/oracle/ssh/known_hosts.jsonl` (`tools/oracle/ssh-probes/run.sh`).
mod support;
use marathon_apple::cmd::remote::ssh::sshj::known_hosts::{
    HostKeyVerifier, OpenSshKnownHosts, PromiscuousVerifier, md5_fingerprint,
};
use russh::keys::PublicKey;
use std::path::Path;
use support::ssh;

fn rows(name: &str) -> Vec<serde_json::Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/oracle/ssh")
        .join(name);
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn known_hosts_matching_follows_the_sshj_oracle() {
    let (logging, _) = ssh::logging(false);
    let mut checked = 0;
    for row in rows("known_hosts.jsonl") {
        let Some(key) = row["key"].as_str() else { continue };
        if row["case"] == "fingerprint" {
            continue;
        }
        let key = PublicKey::from_openssh(key).unwrap();
        if let Some(class) = row["error"].as_str() {
            // The Java constructor threw (an invalid wildcard regex), whatever the file's other lines.
            let text = row["known_hosts"].as_str().unwrap();
            let error = OpenSshKnownHosts::parse(Path::new("known_hosts"), text, logging.clone())
                .err()
                .unwrap_or_else(|| panic!("{} parsed", row["case"]));
            assert_eq!(error.class, class, "{}", row["case"]);
            checked += 1;
            continue;
        }
        let known = match row["known_hosts"].as_str() {
            Some(text) => OpenSshKnownHosts::parse(Path::new("known_hosts"), text, logging.clone()).unwrap(),
            None => OpenSshKnownHosts::load(Path::new("/nonexistent/known_hosts"), logging.clone()).unwrap(),
        };
        let host = row["host"].as_str().unwrap();
        let port = row["port"].as_u64().unwrap() as u16;
        assert_eq!(
            known.verify(host, port, &key),
            row["verify"].as_bool().unwrap(),
            "verify {} {host}:{port}",
            row["case"]
        );
        let algorithms: Vec<String> = serde_json::from_value(row["algorithms"].clone()).unwrap();
        assert_eq!(
            known.find_existing_algorithms(host, port),
            algorithms,
            "algorithms {}",
            row["case"]
        );
        checked += 1;
    }
    // 40 cases on four targets, the constructor that threw, and the missing file.
    assert_eq!(checked, 40 * 4 + 1 + 1);
}

#[test]
fn fingerprint_is_sshj_colon_separated_md5() {
    let row = rows("known_hosts.jsonl")
        .into_iter()
        .find(|row| row["case"] == "fingerprint")
        .unwrap();
    let key = PublicKey::from_openssh(row["key"].as_str().unwrap()).unwrap();
    assert_eq!(md5_fingerprint(&key), row["fingerprint"].as_str().unwrap());
}

#[test]
fn unparseable_lines_are_logged_like_sshj() {
    let (logging, events) = ssh::logging(true);
    let text = "   \n[localhost]:2222 ssh-ed25519\n[localhost]:2222 ssh-foo AAAA\n|1|abc ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGHFJiLi8foDtOdrjRFCdmC6Vvhl3w3sO7bjMUMnBxAR\n[localhost]:2222 ssh-ed25519 !!!!\n";
    OpenSshKnownHosts::parse(Path::new("known_hosts"), text, logging).unwrap();
    let events = events.lock().unwrap();
    let lines: Vec<_> = events
        .iter()
        .map(|(logger, level, message, _)| (logger.as_str(), *level, message.as_str()))
        .collect();
    use jcompat::logging::Level::*;
    let logger = "net.schmizz.sshj.transport.verification.OpenSSHKnownHosts";
    assert_eq!(
        lines,
        [
            (logger, Error, "Error reading entry `   `"),
            (logger, Error, "Error reading entry `[localhost]:2222 ssh-ed25519`"),
            (
                logger,
                Error,
                "Error reading entry `[localhost]:2222 ssh-foo AAAA`, could not determine type"
            ),
            (
                logger,
                Debug,
                "Bad line (net.schmizz.sshj.common.SSHException: Unrecognized format for hashed hostname): |1|abc ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGHFJiLi8foDtOdrjRFCdmC6Vvhl3w3sO7bjMUMnBxAR "
            ),
            (logger, Warn, "Error decoding Base64 key bytes"),
        ]
    );
    let base64 = events[4].3.as_ref().unwrap();
    assert_eq!(base64.class, "net.schmizz.sshj.common.Base64DecodingException".into());
    assert_eq!(
        base64.message,
        Some("Failed to decode base64: Illegal base64 character 21".into())
    );
}

#[test]
fn rejected_wildcard_regex_names_the_pattern_and_the_known_hosts_line() {
    // D6: a wildcard the regex crate rejects (bad syntax, or Java-only syntax such as a backreference) fails
    // parse/load with PatternSyntaxException whose message names the compiled pattern, the file and the line.
    use marathon_apple::cmd::remote::ssh::sshj::error::PATTERN_SYNTAX_EXCEPTION;
    let (logging, _) = ssh::logging(false);
    let key = rows("known_hosts.jsonl")[0]["key"].as_str().unwrap().to_owned();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("known_hosts");
    for (entry, compiled) in [("worker(*", "^worker(.*$"), (r"worker(\w)\1*", r"^worker(\w)\1.*$")] {
        let text = format!("valid-host {key}\n# comment\nother,{entry} {key}\n");
        let expected = |file: &Path| {
            jcompat::JavaRegex::compile(compiled, &format!("{} line 3", file.display()))
                .unwrap_err()
                .to_string()
        };
        let error = OpenSshKnownHosts::parse(Path::new("known_hosts"), &text, logging.clone())
            .err()
            .unwrap_or_else(|| panic!("{entry} parsed"));
        assert_eq!(error.class, PATTERN_SYNTAX_EXCEPTION, "{entry}");
        let message = error.message().unwrap();
        assert_eq!(message, expected(Path::new("known_hosts")), "{entry}");
        assert!(message.contains("known_hosts line 3"), "{entry}: {message}");
        assert!(message.contains(compiled), "{entry}: {message}");
        std::fs::write(&path, &text).unwrap();
        let error = OpenSshKnownHosts::load(&path, logging.clone()).err().unwrap();
        assert_eq!(error.class, PATTERN_SYNTAX_EXCEPTION, "{entry}");
        assert_eq!(error.message().unwrap(), expected(&path), "{entry}");
    }
}

#[test]
fn promiscuous_verifier_accepts_every_key_and_knows_no_algorithms() {
    let key = PublicKey::from_openssh(rows("known_hosts.jsonl")[0]["key"].as_str().unwrap()).unwrap();
    assert!(PromiscuousVerifier.verify("anything", 1, &key));
    assert!(PromiscuousVerifier.find_existing_algorithms("anything", 1).is_empty());
}
