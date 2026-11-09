//! Port of sshj 0.39.0 `net/schmizz/sshj/transport/verification/{HostKeyVerifier,PromiscuousVerifier,OpenSSHKnownHosts}.java`
//! and `com/hierynomus/sshj/transport/verification/KnownHostMatchers.java`, the verifiers that
//! device/ConnectionFactory.kt (4.3) passes to SshjCommandExecutorFactory.connect.
//!
//! sshj's measured rules (T17), not OpenSSH's: unparseable lines are skipped, any matching key of the same type
//! accepts, `!pattern` matches every *other* host, the host is lowercased but patterns are not. Approved P4:
//! `@cert-authority` entries never apply because no certificate host-key algorithm is proposed; the SSH-1
//! `bits e n` form is skipped as unparseable.

use super::config::SshLogging;
use super::error::{FILE_NOT_FOUND_EXCEPTION, PATTERN_SYNTAX_EXCEPTION, SshFailure, io_failure};
use base64::{
    Engine,
    alphabet::STANDARD,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
    prelude::BASE64_STANDARD,
};
use hmac::{Hmac, KeyInit, Mac};
use jcompat::{
    JavaRegex,
    logging::{Level, Logger},
};
use marathon_error::{FailureKind, SshKind};
use russh::keys::PublicKey;
use std::path::{Path, PathBuf};

/// `java.util.Base64.getDecoder()`: padding optional, trailing bits ignored.
const JAVA_BASE64: GeneralPurpose = GeneralPurpose::new(
    &STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);
/// sshj's `Base64Decoder.decode` failure around the JDK's `IllegalArgumentException`.
fn base64_failure(error: &base64::DecodeError) -> SshFailure {
    let detail = match error {
        base64::DecodeError::InvalidByte(_, byte) => format!("Illegal base64 character {byte:x}"),
        other => other.to_string(),
    };
    SshFailure::new(
        FailureKind::Ssh(SshKind::Base64Decoding),
        format!("Failed to decode base64: {detail}"),
    )
    .caused_by(SshFailure::new(FailureKind::IllegalArgument, detail))
}

pub trait HostKeyVerifier: Send + Sync {
    fn verify(&self, hostname: &str, port: u16, key: &PublicKey) -> bool;
    /// Key types already known for the host; sshj moves them to the front of its host-key proposal.
    fn find_existing_algorithms(&self, hostname: &str, port: u16) -> Vec<String>;
    /// `toString()`, which KeyExchanger's rejection log prints.
    fn describe(&self) -> String;
}

pub struct PromiscuousVerifier;
impl HostKeyVerifier for PromiscuousVerifier {
    fn verify(&self, _: &str, _: u16, _: &PublicKey) -> bool {
        true
    }
    fn find_existing_algorithms(&self, _: &str, _: u16) -> Vec<String> {
        Vec::new()
    }
    fn describe(&self) -> String {
        "net.schmizz.sshj.transport.verification.PromiscuousVerifier".into()
    }
}

/// sshj `KeyType` names; anything else is `UNKNOWN`.
const KEY_TYPES: &[&str] = &[
    "ssh-rsa",
    "ssh-dss",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
    "ssh-ed25519",
    "ssh-rsa-cert-v01@openssh.com",
    "ssh-dss-cert-v01@openssh.com",
    "ssh-ed25519-cert-v01@openssh.com",
    "ecdsa-sha2-nistp256-cert-v01@openssh.com",
    "ecdsa-sha2-nistp384-cert-v01@openssh.com",
    "ecdsa-sha2-nistp521-cert-v01@openssh.com",
];
fn is_certificate(key_type: &str) -> bool {
    key_type.ends_with("-cert-v01@openssh.com")
}
/// `KeyType.fromKey`: the blob's type name; RSA is `ssh-rsa` whatever signature hash was negotiated.
pub fn key_type_of(key: &PublicKey) -> Option<&'static str> {
    let name = key.algorithm().as_str().to_owned();
    let name = if name.starts_with("rsa-sha2-") {
        "ssh-rsa".into()
    } else {
        name
    };
    KEY_TYPES.iter().copied().find(|t| *t == name && !is_certificate(t))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Marker {
    CertAuthority,
    Revoked,
}

enum Matcher {
    Equi(String),
    Any(Vec<Matcher>),
    Negate(Box<Matcher>),
    Hashed { hash: String, salt: String },
    Wildcard(JavaRegex),
}
/// Why `createMatcher` gave no matcher.
enum Unmatchable {
    /// SSHException("Unrecognized format for hashed hostname"): the line is skipped.
    HashedFormat,
    /// PatternSyntaxException, unchecked: it escapes the `OpenSSHKnownHosts` constructor.
    Pattern(SshFailure),
}
impl Matcher {
    /// `field` names the known_hosts line for a rejected wildcard's error (D6).
    fn create(entry: &str, field: &str) -> Result<Self, Unmatchable> {
        if entry.contains(',') {
            return java_split(entry, ',')
                .into_iter()
                .map(|entry| Self::create(entry, field))
                .collect::<Result<_, _>>()
                .map(Self::Any);
        }
        if let Some(rest) = entry.strip_prefix('!') {
            return Ok(Self::Negate(Box::new(Self::create(rest, field)?)));
        }
        if entry.starts_with("|1|") {
            let parts = java_split(entry, '|');
            if parts.len() != 4 {
                return Err(Unmatchable::HashedFormat);
            }
            return Ok(Self::Hashed {
                hash: entry.into(),
                salt: parts[2].into(),
            });
        }
        if entry.contains(['*', '?']) {
            // WildcardHostMatcher escapes only `[`, `]` and `.`: any other regex syntax in the entry stays live,
            // so `worker(1|2)*` is a group. Compiled by the regex crate (D6): the wrapper's error, naming the
            // compiled pattern and the known_hosts file and line, replaces Java's text.
            let pattern = format!(
                "^{}$",
                entry
                    .replace('[', "\\[")
                    .replace(']', "\\]")
                    .replace('.', "\\.")
                    .replace('*', ".*")
                    .replace('?', ".")
            );
            return JavaRegex::compile(&pattern, field)
                .map(Self::Wildcard)
                .map_err(|error| Unmatchable::Pattern(SshFailure::new(PATTERN_SYNTAX_EXCEPTION, error.to_string())));
        }
        Ok(Self::Equi(entry.into()))
    }
    fn matches(&self, host: &str, warn: &dyn Fn(String, SshFailure)) -> bool {
        match self {
            Self::Equi(entry) => entry == host,
            Self::Any(matchers) => matchers.iter().any(|m| m.matches(host, warn)),
            Self::Negate(matcher) => !matcher.matches(host, warn),
            Self::Hashed { hash, salt } => {
                let key = match JAVA_BASE64.decode(salt) {
                    Ok(key) => key,
                    Err(error) => {
                        warn(
                            format!("Hostname [{host}] not matched: salt decoding failed"),
                            base64_failure(&error),
                        );
                        return false;
                    }
                };
                let Ok(mut mac) = <Hmac<sha1::Sha1> as KeyInit>::new_from_slice(&key) else {
                    return false;
                };
                mac.update(host.as_bytes());
                *hash == format!("|1|{salt}|{}", BASE64_STANDARD.encode(mac.finalize().into_bytes()))
            }
            Self::Wildcard(pattern) => pattern.matches(host),
        }
    }
}
/// `String.split(String regex)` for one literal character: trailing empty strings are removed.
fn java_split(value: &str, separator: char) -> Vec<&str> {
    let mut parts: Vec<&str> = value.split(separator).collect();
    while parts.len() > 1 && parts.last() == Some(&"") {
        parts.pop();
    }
    if parts == [""] && !value.is_empty() {
        parts.clear();
    }
    parts
}

struct HostEntry {
    marker: Option<Marker>,
    key_type: String,
    key: Vec<u8>,
    matcher: Matcher,
}
impl HostEntry {
    fn applies_to(&self, host: &str, warn: &dyn Fn(String, SshFailure)) -> bool {
        self.matcher.matches(host, warn)
    }
    fn applies_to_type(&self, key_type: &str, host: &str, warn: &dyn Fn(String, SshFailure)) -> bool {
        (self.key_type == key_type || (self.marker == Some(Marker::CertAuthority) && is_certificate(key_type)))
            && self.matcher.matches(host, warn)
    }
    fn verify(&self, key: &[u8]) -> bool {
        key == self.key && self.marker != Some(Marker::Revoked)
    }
}

pub struct OpenSshKnownHosts {
    file: PathBuf,
    entries: Vec<HostEntry>,
    logging: SshLogging,
    logger: Logger,
    matchers: Logger,
}
impl OpenSshKnownHosts {
    /// `OpenSSHKnownHosts(File)`: blocking file I/O, so 4.3 calls it on the vendor runtime's blocking pool.
    /// A missing file behaves as an empty one.
    pub fn load(file: &Path, logging: SshLogging) -> Result<Self, SshFailure> {
        let text = if file.exists() {
            std::fs::read(file).map_err(|e| {
                let mut failure = io_failure(FILE_NOT_FOUND_EXCEPTION, &e);
                failure = SshFailure::new(
                    FILE_NOT_FOUND_EXCEPTION,
                    format!("{} ({})", file.display(), failure.message().unwrap_or_default()),
                );
                failure
            })?
        } else {
            Vec::new()
        };
        Self::parse(file, &String::from_utf8_lossy(&text), logging)
    }
    /// Fails like the Java constructor on a wildcard entry that is not a valid regex.
    pub fn parse(file: &Path, text: &str, logging: SshLogging) -> Result<Self, SshFailure> {
        // LoggerFactory.DEFAULT: a plain slf4j logger, not one MarathonLogging configured.
        let (logger, matchers) = {
            let registry = logging.registry.lock().unwrap_or_else(|e| e.into_inner());
            (
                registry.slf4j_logger("net.schmizz.sshj.transport.verification.OpenSSHKnownHosts"),
                registry.slf4j_logger("com.hierynomus.sshj.transport.verification.KnownHostMatchers"),
            )
        };
        let mut known = Self {
            file: file.into(),
            entries: Vec::new(),
            logging,
            logger,
            matchers,
        };
        for (index, line) in java_lines(text).into_iter().enumerate() {
            if let Some(entry) = known.parse_entry(line, index + 1)? {
                known.entries.push(entry);
            }
        }
        Ok(known)
    }
    fn error(&self, message: String) {
        self.logging.log(&self.logger, Level::Error, || message, None);
    }
    fn warn_matcher(&self, message: String, cause: SshFailure) {
        self.logging.log(&self.matchers, Level::Warn, || message, Some(&cause));
    }
    fn parse_entry(&self, line: &str, number: usize) -> Result<Option<HostEntry>, SshFailure> {
        if line.is_empty() || line.starts_with('#') {
            return Ok(None);
        }
        let trimmed = line.trim_matches(|c: char| c <= ' ');
        let min_components = if trimmed.starts_with('@') { 4 } else { 3 };
        let split = java_whitespace_split(trimmed, min_components + 1);
        if split.len() < min_components {
            self.error(format!("Error reading entry `{line}`"));
            return Ok(None);
        }
        let mut i = 0;
        let marker = match split[0] {
            "@cert-authority" => Some(Marker::CertAuthority),
            "@revoked" => Some(Marker::Revoked),
            _ => None,
        };
        if marker.is_some() {
            i += 1;
        }
        let hostnames = split[i];
        let key_type = split[i + 1];
        i += 2;
        if !KEY_TYPES.contains(&key_type) {
            // Includes the SSH-1 `bits e n` form (approved P4), which sshj would parse as RSA.
            if key_type.parse::<i32>().is_err() {
                self.error(format!("Error reading entry `{line}`, could not determine type"));
            }
            return Ok(None);
        }
        let key = match JAVA_BASE64.decode(match split.get(i) {
            Some(key) => key,
            None => return Ok(None),
        }) {
            Ok(key) if PublicKey::from_bytes(&key).is_ok() => key,
            result => {
                let cause = match result {
                    Err(error) => base64_failure(&error),
                    Ok(_) => SshFailure::new(FailureKind::Ssh(SshKind::Buffer), "Bad item length"),
                };
                self.logging.log(
                    &self.logger,
                    Level::Warn,
                    || "Error decoding Base64 key bytes".into(),
                    Some(&cause),
                );
                return Ok(None);
            }
        };
        let field = format!("{} line {number}", self.file.display());
        let matcher = match Matcher::create(hostnames, &field) {
            Ok(matcher) => matcher,
            Err(Unmatchable::Pattern(failure)) => return Err(failure),
            Err(Unmatchable::HashedFormat) => {
                self.logging.log(
                    &self.logger,
                    Level::Debug,
                    || {
                        format!(
                            "Bad line ({}: Unrecognized format for hashed hostname): {line} ",
                            FailureKind::Ssh(SshKind::General)
                        )
                    },
                    None,
                );
                return Ok(None);
            }
        };
        Ok(Some(HostEntry {
            marker,
            key_type: key_type.into(),
            key,
            matcher,
        }))
    }
    fn adjust_hostname(hostname: &str, port: u16) -> String {
        let lower = hostname.to_lowercase();
        if port != 22 { format!("[{lower}]:{port}") } else { lower }
    }
}
impl HostKeyVerifier for OpenSshKnownHosts {
    fn verify(&self, hostname: &str, port: u16, key: &PublicKey) -> bool {
        let Some(key_type) = key_type_of(key) else {
            return false;
        };
        let Ok(blob) = key.to_bytes() else {
            return false;
        };
        let host = Self::adjust_hostname(hostname, port);
        let warn = |message, cause| self.warn_matcher(message, cause);
        let mut found = false;
        for entry in &self.entries {
            if entry.applies_to_type(key_type, &host, &warn) {
                found = true;
                if entry.verify(&blob) {
                    return true;
                }
            }
        }
        if found {
            self.logging.log(
                &self.logger,
                Level::Warn,
                || format!("Host key for `{host}` has changed!"),
                None,
            );
        }
        false
    }
    fn find_existing_algorithms(&self, hostname: &str, port: u16) -> Vec<String> {
        let host = Self::adjust_hostname(hostname, port);
        let mut algorithms = Vec::new();
        let warn = |message, cause| self.warn_matcher(message, cause);
        for entry in self.entries.iter().filter(|entry| entry.applies_to(&host, &warn)) {
            if entry.marker == Some(Marker::CertAuthority) {
                algorithms.extend(KEY_TYPES.iter().filter(|t| is_certificate(t)).map(|t| t.to_string()));
            } else {
                algorithms.push(entry.key_type.clone());
            }
        }
        algorithms
    }
    fn describe(&self) -> String {
        format!("OpenSSHKnownHosts{{khFile='{}'}}", self.file.display())
    }
}

/// `BufferedReader.readLine`: `\n`, `\r` and `\r\n` end a line; a final unterminated line counts.
fn java_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        match rest.find(['\n', '\r']) {
            Some(end) => {
                lines.push(&rest[..end]);
                let skip = if rest[end..].starts_with("\r\n") { 2 } else { 1 };
                rest = &rest[end + skip..];
            }
            None => {
                lines.push(rest);
                break;
            }
        }
    }
    lines
}
/// `String.split("\\s+", limit)` on an already trimmed string.
fn java_whitespace_split(value: &str, limit: usize) -> Vec<&str> {
    let is_space = |c: char| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r');
    let mut parts = Vec::new();
    let mut rest = value;
    while parts.len() + 1 < limit {
        match rest.find(is_space) {
            Some(start) => {
                parts.push(&rest[..start]);
                let after = &rest[start..];
                rest = after.trim_start_matches(is_space);
            }
            None => break,
        }
    }
    parts.push(rest);
    parts
}

/// sshj's `SecurityUtils.getFingerprint`: colon-separated MD5 of the key blob.
pub fn md5_fingerprint(key: &PublicKey) -> String {
    let blob = key.to_bytes().unwrap_or_default();
    let digest = md5::compute(blob);
    digest
        .0
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}
