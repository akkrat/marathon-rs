//! Private askpass entry point for the controller binary.

use std::{
    io::{self, Write},
    os::unix::ffi::OsStrExt,
    path::PathBuf,
    process::ExitCode,
    sync::atomic::{AtomicBool, Ordering},
};

pub const PASSWORD_ENV: &str = "MARATHON_RSYNC_ASKPASS_SECRET";

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Call before entering the CLI. OpenSSH invokes the same binary with its prompt as the
/// only argument. An ordinary controller instance makes this entry point available to its
/// own transfer launcher.
pub fn enter() -> Option<ExitCode> {
    let Some(password) = std::env::var_os(PASSWORD_ENV)
        .filter(|_| std::env::var_os("SSH_ASKPASS_REQUIRE").is_some_and(|mode| mode == "force"))
    else {
        ENABLED.store(true, Ordering::Release);
        return None;
    };
    let mut args = std::env::args_os().skip(1);
    let accepted = args.next().is_some_and(|prompt| {
        let prompt = prompt.to_string_lossy();
        let prompt = prompt.trim_end().to_ascii_lowercase();
        prompt.ends_with("password:")
            || prompt
                .rsplit_once("password for ")
                .is_some_and(|(_, account)| account.ends_with(':'))
    }) && args.next().is_none()
        && std::env::var_os("SSH_ASKPASS_PROMPT").is_none_or(|hint| hint != "confirm");
    if !accepted {
        return Some(ExitCode::FAILURE);
    }
    let mut output = io::stdout().lock();
    let result = output
        .write_all(password.as_bytes())
        .and_then(|()| output.write_all(b"\n"));
    Some(if result.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Library callers can supply the same contract with a temporary helper when the containing
/// executable has no askpass entry point.
pub fn controller_executable() -> io::Result<Option<PathBuf>> {
    ENABLED.load(Ordering::Acquire).then(std::env::current_exe).transpose()
}
