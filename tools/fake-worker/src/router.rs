//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/WriteFile.kt
//! Also recognizes CommandExecutor.safeExecuteNohup and RemoteFileManager wire commands.
use crate::{
    fs::VirtualFs,
    script::{Action, Script, glob},
};
use anyhow::{Result, bail, ensure};
use base64::{Engine, prelude::BASE64_STANDARD};
use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc};

#[derive(Clone, Debug)]
pub struct Request {
    pub raw: Vec<u8>,
    pub text: String,
    pub argv: Vec<String>,
}
impl Request {
    pub fn parse(raw: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(raw)?.to_owned();
        let argv = shlex::split(&text).ok_or_else(|| anyhow::anyhow!("malformed shell quotes (A12)"))?;
        ensure!(!argv.is_empty(), "empty command");
        Ok(Self {
            raw: raw.to_vec(),
            text,
            argv,
        })
    }
}
pub type HandlerFuture<'a> = Pin<Box<dyn Future<Output = Result<Option<Action>>> + Send + 'a>>;
/// Extensions receive original wire bytes and parsed argv; no SSH types escape.
pub trait Handler: Send + Sync {
    fn name(&self) -> &str;
    fn route<'a>(&'a self, request: &'a Request, fs: &'a VirtualFs) -> HandlerFuture<'a>;
}
#[derive(Clone)]
pub struct Router {
    pub script: Script,
    pub rsync_remote: String,
    pub rsync_binary: PathBuf,
    pub extensions: Vec<Arc<dyn Handler>>,
}
pub enum Plan {
    SignalPid { pid: u64, signal: String, wait: bool },
    Action(Action),
    Rsync(Vec<String>),
    Background { pidfile: String, request: Request },
    BackgroundLog { request: Request },
}
pub struct Routed {
    pub name: String,
    pub plan: Plan,
    pub unmatched: bool,
}
impl Router {
    pub async fn route(&self, r: &Request, fs: &VirtualFs) -> Result<Routed> {
        for rule in &self.script.rules {
            if rule.matcher.matches(&r.text) {
                let plan = if matches!(rule.action, Action::Filesystem) {
                    self.builtin(r, fs)
                        .await?
                        .ok_or_else(|| anyhow::anyhow!("no filesystem operation"))?
                } else {
                    Plan::Action(rule.action.clone())
                };
                return Ok(Routed {
                    name: rule.name.clone(),
                    plan,
                    unmatched: false,
                });
            }
        }
        for handler in &self.extensions {
            if let Some(action) = handler.route(r, fs).await? {
                return Ok(Routed {
                    name: handler.name().into(),
                    plan: Plan::Action(action),
                    unmatched: false,
                });
            }
        }
        if let Some(plan) = self.builtin(r, fs).await? {
            return Ok(Routed {
                name: "builtin".into(),
                plan,
                unmatched: false,
            });
        }
        Ok(Routed {
            name: "unmatched".into(),
            plan: Plan::Action(Action::output("", self.script.unmatched_status.max(1))),
            unmatched: true,
        })
    }
    async fn builtin(&self, r: &Request, fs: &VirtualFs) -> Result<Option<Plan>> {
        let a = r.argv.iter().map(String::as_str).collect::<Vec<_>>();
        if a.first() == Some(&self.rsync_remote.as_str()) {
            return Ok(Some(Plan::Rsync(rsync_args(&a[1..])?)));
        }
        if let ["kill", "-s", signal, pid] = a.as_slice() {
            return Ok(Some(Plan::SignalPid {
                pid: pid.parse()?,
                signal: signal.to_string(),
                wait: false,
            }));
        }
        if let ["sh", "-c", script] = a.as_slice() {
            if let Some(pid) = script.strip_prefix("kill -s INT ").and_then(|s| s.parse::<u64>().ok())
                && *script == format!("kill -s INT {pid}")
            {
                return Ok(Some(Plan::SignalPid {
                    pid,
                    signal: "INT".into(),
                    wait: true,
                }));
            }
            if let Some(nested) = script.strip_suffix(" & echo $!")
                && nested.starts_with("xcrun simctl spawn ")
                && nested.contains(" log stream --type log --color none --style compact 2>/dev/null > ")
            {
                return Ok(Some(Plan::BackgroundLog {
                    request: Request::parse(nested.as_bytes())?,
                }));
            }
            if let Some(rest) = script.strip_prefix("kill -s INT $(cat ") {
                let (pidfile, _) = rest
                    .split_once(')')
                    .ok_or_else(|| anyhow::anyhow!("invalid PID wrapper"))?;
                let exact = format!("kill -s INT $(cat {pidfile}) && lsof -p $(cat {pidfile}) +r 1 &>/dev/null");
                ensure!(*script == exact, "unsupported PID wrapper");
                let pid = String::from_utf8(fs.read(pidfile).await?)?.trim().parse()?;
                return Ok(Some(Plan::SignalPid {
                    pid,
                    signal: "INT".into(),
                    wait: true,
                }));
            }

            if script.starts_with("umask 077 && echo ") {
                let result = write_file(script, fs).await;
                return Ok(Some(Plan::Action(Action::output(
                    "",
                    if result.is_ok() { 0 } else { 1 },
                ))));
            }
            if let Some(rest) = script.strip_prefix("nohup ") {
                let (nested, pidfile) = rest
                    .rsplit_once(" > /dev/null 2>&1 & echo $! > ")
                    .ok_or_else(|| anyhow::anyhow!("unsupported nohup wrapper"))?;
                ensure!(!pidfile.contains(char::is_whitespace), "malformed pid path");
                return Ok(Some(Plan::Background {
                    pidfile: pidfile.into(),
                    request: Request::parse(nested.as_bytes())?,
                }));
            }
            // Shell grammar is deliberately finite. Script rules can handle pipelines
            // by their complete wire string; no host shell is an implicit fallback.
            let nested = Request::parse(script.as_bytes())?;
            if nested.argv.first().is_some_and(|s| s == "sh") {
                bail!("nested sh recursion unsupported");
            }
            return Box::pin(self.builtin(&nested, fs)).await;
        }
        let result = match a.as_slice() {
            ["mkdir", "-p", p] => fs.mkdir(p).await.map(|_| 0),
            ["rm", "-rf" | "-R" | "-f", p] => fs.remove(p).await.map(|_| 0),
            ["cp", "-R", s, d] => fs.copy(s, d).await.map(|_| 0),
            ["mv", "-f", s, d] => fs.rename(s, d).await.map(|_| 0),
            ["ln", "-s", s, d] => fs.symlink(s, d).await.map(|_| 0),
            ["test", flag @ ("-e" | "-f"), p] => Ok(match fs.stat(p).await {
                Ok(e) if *flag == "-e" || e.kind == "file" => 0,
                _ => 1,
            }),
            ["find", p, "-mindepth", "1", "-maxdepth", "1", rest @ ..] => {
                ensure!(rest.ends_with(&["-exec", "rm", "-rf", "{}", "+"]), "unsupported find");
                let rest = &rest[..rest.len() - 5];
                ensure!(rest.len() % 3 == 0, "unsupported find exclusions");
                let mut patterns = vec![];
                for chunk in rest.chunks(3) {
                    ensure!(chunk[..2] == ["!", "-name"], "unsupported find exclusion");
                    patterns.push(chunk[2]);
                }
                for e in fs.list(p).await? {
                    let name = e.path.rsplit('/').next().unwrap_or("");
                    if !patterns.iter().any(|p| glob(p.as_bytes(), name.as_bytes())) {
                        fs.remove(&e.path).await?;
                    }
                }
                Ok(0)
            }
            _ => return Ok(None),
        };
        Ok(Some(Plan::Action(Action::output("", result.unwrap_or(1)))))
    }
}
async fn write_file(script: &str, fs: &VirtualFs) -> Result<()> {
    let rest = script
        .strip_prefix("umask 077 && echo ")
        .ok_or_else(|| anyhow::anyhow!("WriteFile prefix"))?;
    let (encoded, rest) = rest
        .split_once(" | base64 --decode > ")
        .ok_or_else(|| anyhow::anyhow!("WriteFile base64"))?;
    let (temporary, rest) = rest
        .split_once(" && [ $(wc -c < ")
        .ok_or_else(|| anyhow::anyhow!("WriteFile temporary"))?;
    let dst = temporary
        .strip_suffix(".marathon-tmp")
        .ok_or_else(|| anyhow::anyhow!("WriteFile suffix"))?;
    let size = rest
        .split_once(" -eq ")
        .and_then(|(_, r)| r.split_once(" ]"))
        .ok_or_else(|| anyhow::anyhow!("WriteFile size"))?
        .0
        .parse::<usize>()?;
    let exact = format!(
        "umask 077 && echo {encoded} | base64 --decode > {temporary} && [ $(wc -c < {temporary}) -eq {size} ] && mv -f {temporary} {dst} || {{ rm -f {temporary}; exit 1; }}"
    );
    ensure!(script == exact, "unrecognized WriteFile script");
    ensure!(
        dst.starts_with('/') && dst.bytes().all(|b| b.is_ascii_alphanumeric() || b"_./@+-".contains(&b)),
        "nonplain path"
    );
    let decoded = BASE64_STANDARD.decode(encoded)?;
    fs.write_file(dst, decoded, size).await
}
/// Permit precisely the generated openrsync/Samba server dialects, never paths
/// hidden in arbitrary options (e.g. --log-file, --temp-dir, --files-from).
pub fn rsync_args(args: &[&str]) -> Result<Vec<String>> {
    ensure!(args.first() == Some(&"--server"), "rsync requires --server");
    let dot = args
        .iter()
        .position(|a| *a == ".")
        .ok_or_else(|| anyhow::anyhow!("missing rsync operand separator"))?;
    ensure!(args.len() == dot + 2, "one remote rsync operand required");
    let mut i = 1;
    while i < dot {
        let a = args[i];
        if a == "--partial-dir" {
            i += 1;
            ensure!(args.get(i) == Some(&".rsync-partial"), "unsafe partial dir");
        } else if [
            "--sender",
            "--delete-before",
            "--delete",
            "--dirs",
            "--delay-updates",
            "--partial",
            "--partial-dir=.rsync-partial",
        ]
        .contains(&a)
            // D31 on a send and D43 on a receive: what the host's rsync 3 passes on. openrsync does not.
            || a == "--timeout=300"
        {
        } else {
            let short = a
                .strip_prefix('-')
                .ok_or_else(|| anyhow::anyhow!("unsupported rsync option {a}"))?;
            let flags = if let Some((flags, features)) = short.split_once("e.") {
                ensure!(
                    matches!(features, "iLsfxCIvu" | "LsfxCIvu"),
                    "unsupported Samba protocol features"
                );
                flags
            } else {
                short
            };
            // L in Samba's e.FEATURES is not the -L/--copy-links option.
            // Accepting it as an arbitrary short flag would escape via links.
            ensure!(
                !flags.is_empty() && flags.bytes().all(|c| b"rlptgoDv".contains(&c)),
                "unsupported rsync server option {a}"
            );
        }
        i += 1;
    }
    Ok(args.iter().map(|s| s.to_string()).collect())
}
