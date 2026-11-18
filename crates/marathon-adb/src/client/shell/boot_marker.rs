//! Shell requests for Android's boot identity and its Marathon preparation marker (D85).

use uuid::Uuid;

pub const BOOT_ID_COMMAND: &str = "cat /proc/sys/kernel/random/boot_id";
const PREFIX: &str = "token=";
const MARKER: &str = r#"marker=/data/local/tmp/.marathon-ready-$token; "#;
const WRITE: &str = r#"(umask 077; printf '%s\n' "$token" > "$marker") 2>/dev/null && printf '%s\n' "$token""#;
// A failed read is only evidence of absence if a directory listing succeeded and lacks the name.
// A shell can be allowed to list an entry that it cannot read. API 21's toolbox ls has no -1.
const READ: &str = r#"printf 'boot '; cat /proc/sys/kernel/random/boot_id 2>/dev/null || printf 'unknown\n'; printf 'marker '; if value=$(cat "$marker" 2>/dev/null); then printf 'value=%s\n' "$value"; elif cd /data/local/tmp 2>/dev/null && entries=$(ls -a 2>/dev/null) && [ -n "$entries" ]; then case "
$entries
" in *"
${marker##*/}
"*) printf 'unknown\n';; *) printf 'missing\n';; esac; else printf 'unknown\n'; fi"#;
const REMOVE: &str = r#"rm -f "$marker""#;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MarkerCommand {
    Write,
    Read,
    Remove,
}

impl MarkerCommand {
    fn body(self) -> &'static str {
        match self {
            Self::Write => WRITE,
            Self::Read => READ,
            Self::Remove => REMOVE,
        }
    }

    pub fn command(self, token: Uuid) -> String {
        format!("{PREFIX}{token}; {MARKER}{}", self.body())
    }

    pub fn parse(command: &str) -> Option<(Self, Uuid)> {
        let (token, body) = command.strip_prefix(PREFIX)?.split_once("; ")?;
        let uuid: Uuid = token.parse().ok()?;
        if uuid.is_nil() || uuid.hyphenated().encode_lower(&mut Uuid::encode_buffer()) != token {
            return None;
        }
        let command = match body.strip_prefix(MARKER)? {
            WRITE => Self::Write,
            READ => Self::Read,
            REMOVE => Self::Remove,
            _ => return None,
        };
        Some((command, uuid))
    }
}
