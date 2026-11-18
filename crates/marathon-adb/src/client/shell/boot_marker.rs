//! Shell requests for Android's boot identity and its Marathon preparation marker (D85).

use uuid::Uuid;

pub const BOOT_ID_COMMAND: &str = "cat /proc/sys/kernel/random/boot_id";
const PREFIX: &str = "token=";
const MARKER: &str = r#"marker=/data/local/tmp/.marathon-ready-$token; "#;
// Android 5.0 and 5.1 provide echo in mksh. Filter the data read from disk before
// echo so that escape sequences in malformed input cannot be interpreted as part of an identity.
const WRITE: &str = r#"(umask 077; echo "$token" > "$marker") 2>/dev/null && echo "$token""#;
// A failed read is only evidence of absence if a directory listing succeeded and lacks the name.
// A shell can be allowed to list an entry that it cannot read. API 21's toolbox ls has no -1.
const READ: &str = r#"boot=$(cat /proc/sys/kernel/random/boot_id 2>/dev/null) || boot=
case "$boot" in
    ''|*[!0123456789abcdefABCDEF-]*) echo 'boot unknown' ;;
    *) echo "boot $boot" ;;
esac
if value=$(cat "$marker" 2>/dev/null); then
    case "$value" in
        ''|*[!0123456789abcdefABCDEF-]*) echo 'marker unknown' ;;
        *) echo "marker value=$value" ;;
    esac
elif cd /data/local/tmp 2>/dev/null && entries=$(ls -a 2>/dev/null) && [ -n "$entries" ]; then
    case "
$entries
" in
        *"
${marker##*/}
"*) echo 'marker unknown' ;;
        *) echo 'marker missing' ;;
    esac
else
    echo 'marker unknown'
fi"#;
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
