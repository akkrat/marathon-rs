//! The device-script DSL (`docs/design/fake-adb-script.md`).
//!
//! A script is YAML (JSON is valid YAML and works too). [`Script`] is the serde model; [`Script::compile`]
//! checks it, resolves file references against the script's directory and loads them, giving the
//! [`CompiledScript`] that the server runs.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use regex::Regex;
use serde::Deserialize;

/// Features a device reports when its script doesn't list any: a recent emulator, minus the compressed
/// sync v2 variants, which the server doesn't implement.
pub const DEFAULT_DEVICE_FEATURES: &[&str] = &[
    "shell_v2",
    "cmd",
    "stat_v2",
    "ls_v2",
    "fixed_push_mkdir",
    "apex",
    "abb",
    "fixed_push_symlink_timestamp",
    "abb_exec",
    "remount_shell",
    "track_app",
    "sendrecv_v2",
];

/// `host:host-features` when the script doesn't override it.
pub const DEFAULT_HOST_FEATURES: &[&str] = &[
    "shell_v2",
    "cmd",
    "stat_v2",
    "ls_v2",
    "fixed_push_mkdir",
    "apex",
    "abb",
    "fixed_push_symlink_timestamp",
    "abb_exec",
    "remount_shell",
    "track_app",
    "sendrecv_v2",
    "sendrecv_v2_brotli",
    "sendrecv_v2_lz4",
    "sendrecv_v2_zstd",
    "sendrecv_v2_dry_run_send",
];

/// ADB_SERVER_VERSION of platform-tools 3x; the real client kills a server that reports another one.
pub const DEFAULT_ADB_VERSION: u32 = 41;

#[derive(Debug)]
pub struct ScriptError(pub String);

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ScriptError {}

fn err<T>(msg: impl Into<String>) -> Result<T, ScriptError> {
    Err(ScriptError(msg.into()))
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Script {
    #[serde(default)]
    pub server: ServerSpec,
    #[serde(default)]
    pub devices: Vec<DeviceSpec>,
    /// Expectations shared by every device, tried after the device's own.
    #[serde(default)]
    pub shell: Vec<ShellExpectation>,
    /// The answer to a command that nothing matches. The server also records a warning.
    #[serde(default)]
    pub default: DefaultResponse,
    #[serde(default)]
    pub events: Vec<EventSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSpec {
    #[serde(default = "default_adb_version")]
    pub adb_version: u32,
    pub host_features: Option<Vec<String>>,
    #[serde(default)]
    pub on_kill: KillBehaviour,
    /// Built-in `getprop` and `echo` for commands no expectation matches.
    #[serde(default = "yes")]
    pub builtins: bool,
    /// Unmatched commands get a FAIL instead of `default`, and `fake-adb` exits non-zero if the transcript
    /// holds any warning.
    #[serde(default)]
    pub strict: bool,
}

impl Default for ServerSpec {
    fn default() -> Self {
        ServerSpec {
            adb_version: DEFAULT_ADB_VERSION,
            host_features: None,
            on_kill: KillBehaviour::Exit,
            builtins: true,
            strict: false,
        }
    }
}

fn default_adb_version() -> u32 {
    DEFAULT_ADB_VERSION
}

fn yes() -> bool {
    true
}

/// What `host:kill` (`adb kill-server`) does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KillBehaviour {
    /// Answer OKAY, drop every connection and stop listening, like the real server.
    #[default]
    Exit,
    /// Answer OKAY and carry on.
    Ignore,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceSpec {
    pub serial: String,
    #[serde(default = "default_state")]
    pub state: String,
    pub features: Option<Vec<String>>,
    #[serde(default)]
    pub getprop: BTreeMap<String, Scalar>,
    #[serde(default)]
    pub files: Vec<FileSpec>,
    #[serde(default)]
    pub shell: Vec<ShellExpectation>,
    pub framebuffer: Option<FramebufferSpec>,
    /// A device with `connected: false` appears only through a `connect` event.
    #[serde(default = "yes")]
    pub connected: bool,
}

fn default_state() -> String {
    "device".to_string()
}

/// A YAML scalar read as text, so that `ro.build.version.sdk: 33` works without quotes.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Scalar {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl fmt::Display for Scalar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scalar::String(s) => f.write_str(s),
            Scalar::Int(i) => write!(f, "{i}"),
            Scalar::Float(x) => write!(f, "{x}"),
            Scalar::Bool(b) => write!(f, "{b}"),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileSpec {
    pub path: String,
    pub content: Option<String>,
    /// A host file whose bytes become the content.
    pub file: Option<PathBuf>,
    #[serde(default)]
    pub dir: bool,
    /// Permission bits as an octal string such as `"0644"`.
    pub mode: Option<String>,
    #[serde(default)]
    pub mtime: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FramebufferSpec {
    /// The protocol version sent first: 1, 2 or 16 (the legacy RGB565 header).
    #[serde(default = "one")]
    pub version: u32,
    /// Everything after the version word (header and pixels), sent unchanged. This is the format of
    /// adam's server-stub replay files such as `fixtures/vendor-android/framebuffer/screencap_1.bin`.
    pub file: Option<PathBuf>,
    /// Raw RGBA_8888 pixels; the header is generated from `width` and `height`.
    pub pixels: Option<PathBuf>,
    /// A solid RGBA colour; the header is generated from `width` and `height`.
    pub fill: Option<[u8; 4]>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    #[serde(default)]
    pub color_space: u32,
}

fn one() -> u32 {
    1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceKind {
    /// `shell:` and `shell,v2,…:`.
    Shell,
    Exec,
    AbbExec,
}

impl ServiceKind {
    pub fn name(self) -> &'static str {
        match self {
            ServiceKind::Shell => "shell",
            ServiceKind::Exec => "exec",
            ServiceKind::AbbExec => "abb_exec",
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellExpectation {
    pub exact: Option<String>,
    pub prefix: Option<String>,
    pub regex: Option<String>,
    /// Restricts the expectation to one service; any by default.
    pub service: Option<ServiceKind>,
    /// The expectation stops matching after this many uses.
    pub times: Option<u32>,
    #[serde(default)]
    pub delay_ms: u64,
    pub stdout: Option<String>,
    pub stdout_file: Option<PathBuf>,
    /// Bytes per write when streaming `stdout_file`; the whole file at once by default.
    pub chunk_size: Option<usize>,
    #[serde(default)]
    pub chunk_interval_ms: u64,
    pub stderr: Option<String>,
    /// Explicit output steps, instead of `stdout`, `stdout_file` and `stderr`.
    pub output: Option<Vec<OutputStep>>,
    #[serde(default)]
    pub exit_code: i32,
    /// Never finish: after the output the command runs until the client closes the connection.
    #[serde(default)]
    pub hang: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum OutputStep {
    Stdout(String),
    Stderr(String),
    StdoutFile(FileStream),
    SleepMs(u64),
    /// Waits until the test opens the gate of this name with `FakeAdbServer::open_gate`.
    Gate(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum FileStream {
    Path(PathBuf),
    Options {
        path: PathBuf,
        chunk_size: Option<usize>,
        #[serde(default)]
        interval_ms: u64,
    },
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultResponse {
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(default)]
    pub exit_code: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EventSpec {
    /// Milliseconds after the server started.
    pub at_ms: u64,
    #[serde(flatten)]
    pub action: EventAction,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventAction {
    /// Connects a device declared in `devices` (usually with `connected: false`), or reconnects one.
    Connect(String),
    Disconnect(String),
    State {
        serial: String,
        state: String,
    },
    /// Drops every connection and stops listening, as if the server died; listens again after
    /// `restart_after_ms` if given.
    KillServer {
        #[serde(default)]
        restart_after_ms: Option<u64>,
    },
}

impl Script {
    pub fn from_yaml(text: &str) -> Result<Script, ScriptError> {
        // Enums are written as one-key maps (`{ stdout: "…" }`, `{ connect: serial }`), not YAML tags.
        serde_norway::with::singleton_map_recursive::deserialize(serde_norway::Deserializer::from_str(text))
            .map_err(|e| ScriptError(format!("invalid device script: {e}")))
    }

    /// Reads and compiles a script file; relative paths in it resolve against its directory.
    pub fn load(path: &Path) -> Result<CompiledScript, ScriptError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ScriptError(format!("cannot read device script {}: {e}", path.display())))?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        Script::from_yaml(&text)?.compile(base)
    }

    pub fn compile(&self, base_dir: &Path) -> Result<CompiledScript, ScriptError> {
        let mut seen = std::collections::HashSet::new();
        let mut devices = Vec::new();
        for d in &self.devices {
            if !seen.insert(d.serial.clone()) {
                return err(format!("device {} is declared twice", d.serial));
            }
            devices.push(compile_device(d, base_dir)?);
        }
        for e in &self.events {
            match &e.action {
                EventAction::Connect(s) | EventAction::Disconnect(s) | EventAction::State { serial: s, .. } => {
                    if !seen.contains(s) {
                        return err(format!("event at {} ms names an undeclared device {s}", e.at_ms));
                    }
                }
                EventAction::KillServer { .. } => {}
            }
        }
        Ok(CompiledScript {
            adb_version: self.server.adb_version,
            host_features: self
                .server
                .host_features
                .clone()
                .unwrap_or_else(|| DEFAULT_HOST_FEATURES.iter().map(|s| s.to_string()).collect()),
            on_kill: self.server.on_kill,
            builtins: self.server.builtins,
            strict: self.server.strict,
            devices,
            shell: compile_rules(&self.shell, base_dir, "shell")?,
            default: Response {
                steps: [
                    (!self.default.stdout.is_empty()).then(|| Step::Stdout(self.default.stdout.clone().into_bytes())),
                    (!self.default.stderr.is_empty()).then(|| Step::Stderr(self.default.stderr.clone().into_bytes())),
                ]
                .into_iter()
                .flatten()
                .collect(),
                exit_code: exit_code(self.default.exit_code, "default")?,
                hang: false,
            },
            events: self.events.clone(),
        })
    }
}

/// A script ready to run: files loaded, regexes built, defaults filled in.
#[derive(Debug, Clone)]
pub struct CompiledScript {
    pub adb_version: u32,
    pub host_features: Vec<String>,
    pub on_kill: KillBehaviour,
    pub builtins: bool,
    pub strict: bool,
    pub devices: Vec<CompiledDevice>,
    pub shell: Vec<Rule>,
    pub default: Response,
    pub events: Vec<EventSpec>,
}

#[derive(Debug, Clone)]
pub struct CompiledDevice {
    pub serial: String,
    pub state: String,
    pub features: Vec<String>,
    pub getprop: BTreeMap<String, String>,
    pub files: Vec<CompiledFile>,
    pub shell: Vec<Rule>,
    pub framebuffer: Option<Arc<Vec<u8>>>,
    pub connected: bool,
}

#[derive(Debug, Clone)]
pub struct CompiledFile {
    pub path: String,
    /// `None` for a directory.
    pub content: Option<Vec<u8>>,
    pub mode: u32,
    pub mtime: u32,
}

#[derive(Debug, Clone)]
pub enum Matcher {
    Exact(String),
    Prefix(String),
    Regex(Regex),
}

impl Matcher {
    pub fn matches(&self, command: &str) -> bool {
        match self {
            Matcher::Exact(s) => command == s,
            Matcher::Prefix(p) => command.starts_with(p.as_str()),
            Matcher::Regex(r) => r.is_match(command),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Rule {
    /// `shell[3]` or `devices[emulator-5554].shell[0]`, for the transcript.
    pub id: String,
    pub matcher: Matcher,
    pub service: Option<ServiceKind>,
    pub times: Option<u32>,
    pub response: Response,
}

#[derive(Debug, Clone)]
pub struct Response {
    pub steps: Vec<Step>,
    pub exit_code: u8,
    pub hang: bool,
}

#[derive(Debug, Clone)]
pub enum Step {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    StdoutStream {
        data: Arc<Vec<u8>>,
        chunk_size: usize,
        interval_ms: u64,
    },
    Sleep(u64),
    Gate(String),
}

fn compile_device(d: &DeviceSpec, base: &Path) -> Result<CompiledDevice, ScriptError> {
    let mut files = Vec::new();
    for f in &d.files {
        if !f.path.starts_with('/') {
            return err(format!("device {}: file path {} is not absolute", d.serial, f.path));
        }
        let sources = [f.content.is_some(), f.file.is_some(), f.dir]
            .iter()
            .filter(|b| **b)
            .count();
        if sources != 1 {
            return err(format!(
                "device {}: file {} needs exactly one of content, file, dir",
                d.serial, f.path
            ));
        }
        let content = if f.dir {
            None
        } else if let Some(c) = &f.content {
            Some(c.clone().into_bytes())
        } else {
            Some(read_file(base, f.file.as_deref().unwrap_or(Path::new("")))?)
        };
        let mode = match &f.mode {
            Some(m) => u32::from_str_radix(m.trim_start_matches("0o"), 8)
                .map_err(|_| ScriptError(format!("device {}: file {}: bad octal mode {m}", d.serial, f.path)))?,
            None if f.dir => 0o755,
            None => 0o644,
        };
        files.push(CompiledFile {
            path: f.path.clone(),
            content,
            mode,
            mtime: f.mtime,
        });
    }
    let mut probe = super::fs::DeviceFs::default();
    for f in &files {
        match &f.content {
            Some(c) => probe.write(&f.path, c.clone(), f.mode, f.mtime),
            None => probe.mkdir(&f.path, f.mode, f.mtime),
        }
        .map_err(|e| ScriptError(format!("device {}: files: {e}", d.serial)))?;
    }
    Ok(CompiledDevice {
        serial: d.serial.clone(),
        state: d.state.clone(),
        features: d
            .features
            .clone()
            .unwrap_or_else(|| DEFAULT_DEVICE_FEATURES.iter().map(|s| s.to_string()).collect()),
        getprop: d.getprop.iter().map(|(k, v)| (k.clone(), v.to_string())).collect(),
        files,
        shell: compile_rules(&d.shell, base, &format!("devices[{}].shell", d.serial))?,
        framebuffer: d
            .framebuffer
            .as_ref()
            .map(|f| compile_framebuffer(f, base, &d.serial))
            .transpose()?
            .map(Arc::new),
        connected: d.connected,
    })
}

fn compile_rules(list: &[ShellExpectation], base: &Path, prefix: &str) -> Result<Vec<Rule>, ScriptError> {
    list.iter()
        .enumerate()
        .map(|(i, e)| compile_rule(e, base, &format!("{prefix}[{i}]")))
        .collect()
}

fn compile_rule(e: &ShellExpectation, base: &Path, id: &str) -> Result<Rule, ScriptError> {
    let matcher = match (&e.exact, &e.prefix, &e.regex) {
        (Some(s), None, None) => Matcher::Exact(s.clone()),
        (None, Some(p), None) => Matcher::Prefix(p.clone()),
        (None, None, Some(r)) => {
            Matcher::Regex(Regex::new(r).map_err(|x| ScriptError(format!("{id}: invalid regex {r:?}: {x}")))?)
        }
        _ => return err(format!("{id}: needs exactly one of exact, prefix, regex")),
    };
    let steps = match &e.output {
        Some(output) => {
            if e.stdout.is_some() || e.stdout_file.is_some() || e.stderr.is_some() {
                return err(format!(
                    "{id}: output can't be combined with stdout, stdout_file or stderr"
                ));
            }
            let mut steps = Vec::new();
            if e.delay_ms > 0 {
                steps.push(Step::Sleep(e.delay_ms));
            }
            for s in output {
                steps.push(match s {
                    OutputStep::Stdout(t) => Step::Stdout(t.clone().into_bytes()),
                    OutputStep::Stderr(t) => Step::Stderr(t.clone().into_bytes()),
                    OutputStep::SleepMs(ms) => Step::Sleep(*ms),
                    OutputStep::Gate(name) => Step::Gate(name.clone()),
                    OutputStep::StdoutFile(FileStream::Path(p)) => stream_step(base, p, None, 0)?,
                    OutputStep::StdoutFile(FileStream::Options {
                        path,
                        chunk_size,
                        interval_ms,
                    }) => stream_step(base, path, *chunk_size, *interval_ms)?,
                });
            }
            steps
        }
        None => {
            let mut steps = Vec::new();
            if e.delay_ms > 0 {
                steps.push(Step::Sleep(e.delay_ms));
            }
            if let Some(s) = &e.stdout {
                steps.push(Step::Stdout(s.clone().into_bytes()));
            }
            if let Some(p) = &e.stdout_file {
                steps.push(stream_step(base, p, e.chunk_size, e.chunk_interval_ms)?);
            }
            if let Some(s) = &e.stderr {
                steps.push(Step::Stderr(s.clone().into_bytes()));
            }
            steps
        }
    };
    if e.times == Some(0) {
        return err(format!("{id}: times must be at least 1"));
    }
    Ok(Rule {
        id: id.to_string(),
        matcher,
        service: e.service,
        times: e.times,
        response: Response {
            steps,
            exit_code: exit_code(e.exit_code, id)?,
            hang: e.hang,
        },
    })
}

fn exit_code(code: i32, id: &str) -> Result<u8, ScriptError> {
    u8::try_from(code).or_else(|_| err(format!("{id}: exit_code {code} is outside 0..=255")))
}

fn stream_step(base: &Path, path: &Path, chunk_size: Option<usize>, interval_ms: u64) -> Result<Step, ScriptError> {
    if chunk_size == Some(0) {
        return err(format!("{}: chunk_size must be at least 1", path.display()));
    }
    let data = read_file(base, path)?;
    let chunk_size = chunk_size.unwrap_or(data.len().max(1));
    Ok(Step::StdoutStream {
        data: Arc::new(data),
        chunk_size,
        interval_ms,
    })
}

fn read_file(base: &Path, path: &Path) -> Result<Vec<u8>, ScriptError> {
    let full = base.join(path);
    std::fs::read(&full).map_err(|e| ScriptError(format!("cannot read {}: {e}", full.display())))
}

fn compile_framebuffer(f: &FramebufferSpec, base: &Path, serial: &str) -> Result<Vec<u8>, ScriptError> {
    let ctx = |m: &str| ScriptError(format!("device {serial}: framebuffer: {m}"));
    let mut out = f.version.to_le_bytes().to_vec();
    match (&f.file, &f.pixels, &f.fill) {
        (Some(file), None, None) => {
            out.extend(read_file(base, file)?);
            return Ok(out);
        }
        (None, Some(_), None) | (None, None, Some(_)) => {}
        _ => return Err(ctx("needs exactly one of file, pixels, fill")),
    }
    let (Some(width), Some(height)) = (f.width, f.height) else {
        return Err(ctx("pixels and fill need width and height"));
    };
    let size = width as usize * height as usize * 4;
    let pixels = match (&f.pixels, f.fill) {
        (Some(p), _) => {
            let data = read_file(base, p)?;
            if data.len() != size {
                return Err(ctx(&format!(
                    "{} has {} bytes, {width}x{height} RGBA needs {size}",
                    p.display(),
                    data.len()
                )));
            }
            data
        }
        (None, Some(rgba)) => rgba.repeat(width as usize * height as usize),
        (None, None) => unreachable!("checked above"),
    };
    // RGBA_8888 as AOSP's framebuffer_service describes it. v2 adds the colour space after bpp; the
    // offsets are red, blue, green, alpha in that order.
    let mut header = vec![32u32];
    match f.version {
        1 => {}
        2 => header.push(f.color_space),
        v => return Err(ctx(&format!("version {v} can only be used with file"))),
    }
    header.extend([size as u32, width, height, 0, 8, 16, 8, 8, 8, 24, 8]);
    for v in header {
        out.extend(v.to_le_bytes());
    }
    out.extend(pixels);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compile(yaml: &str) -> Result<CompiledScript, ScriptError> {
        Script::from_yaml(yaml)?.compile(Path::new("."))
    }

    #[test]
    fn full_script_parses() {
        let s = compile(
            r#"
server: { adb_version: 40, on_kill: ignore, builtins: false, strict: true }
devices:
  - serial: emulator-5554
    getprop: { ro.build.version.sdk: 33, sys.boot_completed: "1" }
    files:
      - { path: /sdcard/a.txt, content: "hi", mode: "0600", mtime: 7 }
      - { path: /sdcard/dir, dir: true }
    shell:
      - { exact: "pm list packages", stdout: "package:a\n" }
      - prefix: "am instrument"
        service: shell
        output: [ { stdout: "a" }, { sleep_ms: 5 }, { stderr: "b" } ]
        exit_code: 1
    framebuffer: { version: 2, width: 2, height: 1, fill: [1, 2, 3, 4] }
  - { serial: emulator-5556, state: offline, features: [cmd], connected: false }
shell:
  - { regex: "^rm .*", times: 2, hang: true }
default: { stdout: "?", exit_code: 127 }
events:
  - { at_ms: 10, connect: emulator-5556 }
  - { at_ms: 20, state: { serial: emulator-5556, state: device } }
  - { at_ms: 30, disconnect: emulator-5554 }
  - { at_ms: 40, kill_server: { restart_after_ms: 5 } }
"#,
        )
        .unwrap();
        assert_eq!(s.adb_version, 40);
        assert_eq!(s.on_kill, KillBehaviour::Ignore);
        assert!(!s.builtins);
        assert!(s.strict);
        let d = &s.devices[0];
        assert_eq!(d.getprop["ro.build.version.sdk"], "33");
        assert_eq!(d.features, DEFAULT_DEVICE_FEATURES);
        assert_eq!(d.files[0].mode, 0o600);
        assert_eq!(d.files[1].mode, 0o755);
        assert!(d.files[1].content.is_none());
        assert_eq!(d.shell[1].id, "devices[emulator-5554].shell[1]");
        assert_eq!(d.shell[1].response.exit_code, 1);
        assert_eq!(d.shell[1].response.steps.len(), 3);
        // version, 13 header words, 2 pixels.
        assert_eq!(d.framebuffer.as_ref().unwrap().len(), 4 + 13 * 4 + 8);
        assert_eq!(s.devices[1].state, "offline");
        assert!(!s.devices[1].connected);
        assert!(s.shell[0].matcher.matches("rm -r /x"));
        assert_eq!(s.shell[0].times, Some(2));
        assert_eq!(s.default.exit_code, 127);
        assert_eq!(s.events.len(), 4);
    }

    #[test]
    fn script_errors_are_reported() {
        let cases = [
            ("devices: [{ serial: a, bogus: 1 }]", "unknown field"),
            ("shell: [{ stdout: x }]", "exactly one of exact, prefix, regex"),
            (
                "shell: [{ exact: a, prefix: b }]",
                "exactly one of exact, prefix, regex",
            ),
            ("shell: [{ regex: '(' }]", "invalid regex"),
            ("shell: [{ exact: a, exit_code: 256 }]", "outside 0..=255"),
            ("shell: [{ exact: a, stdout: x, output: [] }]", "can't be combined"),
            ("devices: [{ serial: a }, { serial: a }]", "declared twice"),
            (
                "devices: [{ serial: a, files: [{ path: rel, content: x }] }]",
                "not absolute",
            ),
            (
                "devices: [{ serial: a, files: [{ path: /x }] }]",
                "exactly one of content",
            ),
            (
                "devices: [{ serial: a, files: [{ path: /f, content: x }, { path: /f/g, content: y }] }]",
                "Not a directory",
            ),
            ("events: [{ at_ms: 1, connect: nope }]", "undeclared device"),
            (
                "devices: [{ serial: a, framebuffer: { fill: [0,0,0,0] } }]",
                "need width and height",
            ),
            ("shell: [{ exact: a, stdout_file: /does/not/exist }]", "cannot read"),
        ];
        for (yaml, expected) in cases {
            let e = compile(yaml).expect_err(yaml);
            assert!(e.0.contains(expected), "{yaml}: {e}");
        }
    }

    #[test]
    fn json_is_accepted() {
        let s = compile(r#"{"devices": [{"serial": "x", "shell": [{"exact": "id", "stdout": "uid=0"}]}]}"#).unwrap();
        assert_eq!(s.devices[0].serial, "x");
    }
}
