//! Versioned harness script. Patterns are shell-style globs, not Java regexes.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Script {
    pub rules: Vec<Rule>,
    pub unmatched_status: u32,
    pub faults: Faults,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub name: String,
    pub matcher: Matcher,
    pub action: Action,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Matcher {
    Exact(String),
    Prefix(String),
    Pattern(String),
}
impl Matcher {
    pub fn matches(&self, s: &str) -> bool {
        match self {
            Self::Exact(p) => p == s,
            Self::Prefix(p) => s.starts_with(p),
            Self::Pattern(p) => glob(p.as_bytes(), s.as_bytes()),
        }
    }
}
pub fn glob(p: &[u8], s: &[u8]) -> bool {
    let (mut pi, mut si, mut star, mut mark) = (0, 0, None, 0);
    while si < s.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            pi += 1;
            mark = si;
        } else if let Some(st) = star {
            mark += 1;
            si = mark;
            pi = st + 1;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Output {
        #[serde(default)]
        chunks: Vec<Chunk>,
        #[serde(default)]
        status: u32,
        #[serde(default)]
        signal: Option<String>,
        #[serde(default)]
        hang: bool,
        #[serde(default)]
        ignore_signals: Vec<String>,
        #[serde(default)]
        eof_before_exit: bool,
        #[serde(default)]
        close_before_eof: bool,
        #[serde(default)]
        withhold_close: bool,
    },
    /// Reuse Output execution, publishing files only after a completed command
    /// or the named background signal. Transport loss publishes nothing.
    Publish {
        output: Box<Action>,
        files: Vec<PublishedFile>,
        #[serde(default)]
        started: Vec<PublishedFile>,
        on_signal: Option<String>,
    },
    EchoStdin,
    Filesystem,
    Reject,
    Disconnect {
        reason: u32,
        message: String,
    },
    DropTransport,
}
impl Action {
    pub fn output(text: &str, status: u32) -> Self {
        Self::Output {
            chunks: vec![Chunk {
                data: text.into(),
                ..Default::default()
            }],
            status,
            signal: None,
            hang: false,
            ignore_signals: vec![],
            eof_before_exit: false,
            close_before_eof: false,
            withhold_close: false,
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Chunk {
    pub data: String,
    pub base64: Option<String>,
    pub fixture: Option<String>,
    pub delay_ms: u64,
    pub extended: Option<u32>,
    pub eof_before: bool,
    pub wait_for_file: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Faults {
    pub withhold_close: bool,
    pub keepalive: Keepalive,
    pub reject_exec: bool,
    /// An `exec` whose string contains this gets no reply and starts nothing, and its session keeps its slot until closed.
    pub silent_exec: Option<String>,
    /// The first session open of a connection gets its reply this late.
    pub late_open: Option<LateOpen>,
    pub before_kex: Option<Disconnect>,
    pub after_auth: Option<Disconnect>,
    pub during_exec: Option<Disconnect>,
    pub no_common_kex: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LateOpen {
    pub delay_millis: u64,
    /// Refuse rather than accept. Only an accepted one holds a slot while it waits.
    pub reject: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Disconnect {
    pub reason: u32,
    pub message: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Keepalive {
    #[default]
    Answer,
    Delay(u64),
    Withhold,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishedFile {
    pub path: String,
    pub base64: String,
}
