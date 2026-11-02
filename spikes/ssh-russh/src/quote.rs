//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/remote/ssh/sshj/SshjCommandExecutor.kt
//! (`shellCommand`) and vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/WriteFile.kt (the script).

use base64::Engine;

const SHELL_SPECIAL_CHARS: &[char] = &[
    '"', ' ', '$', '\'', '\\', '#', '=', '[', ']', '!', '>', '<', '|', ';', '{', '}', '(', ')', '*', '?', '~', '&',
];

/// The command line the worker's login shell runs. Bug-compatible: only the first and last characters decide whether an
/// argument is already quoted, and a `'` inside is "escaped" as `\'`, which a POSIX shell does not honour inside quotes.
pub fn shell_command(command: &[String]) -> String {
    command
        .iter()
        .map(|arg| {
            if is_shellscaped(arg) {
                arg.clone()
            } else if arg.chars().any(|c| SHELL_SPECIAL_CHARS.contains(&c)) {
                format!("'{}'", arg.replace('\'', "\\'"))
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_shellscaped(arg: &str) -> bool {
    (arg.starts_with('\'') && arg.ends_with('\'')) || (arg.starts_with('"') && arg.ends_with('"'))
}

pub const MAX_WRITTEN_FILE_SIZE: usize = 32 * 1024;

/// `/[A-Za-z0-9_./@+-]+`, matched against the whole string
fn is_plain_path(dst: &str) -> bool {
    dst.len() > 1 && dst.starts_with('/') && dst.chars().all(|c| c.is_ascii_alphanumeric() || "_./@+-".contains(c))
}

/// The `sh -c` argv that writeFile sends, or None when it refuses without running anything
pub fn write_file_command(content: &[u8], dst: &str) -> Option<Vec<String>> {
    if content.len() > MAX_WRITTEN_FILE_SIZE || !is_plain_path(dst) {
        return None;
    }
    let temporary = format!("{dst}.marathon-tmp");
    let encoded = base64::engine::general_purpose::STANDARD.encode(content);
    let script = format!(
        "umask 077 && echo {encoded} | base64 --decode > {temporary} && [ $(wc -c < {temporary}) -eq {} ] && \
         mv -f {temporary} {dst} || {{ rm -f {temporary}; exit 1; }}",
        content.len()
    );
    Some(vec!["sh".into(), "-c".into(), script])
}
