//! Fake-worker feasibility (input for WP 0.4a): a russh server that a stock OpenSSH client and rsync can use.
//! Every exec request runs `sh -c <command>` with piped stdio, so `rsync --server` runs for real. Events (auth methods,
//! exec, env, pty, signal, close) are printed as `EVENT` lines.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use russh::keys::{self, PublicKey};
use russh::server::{Auth, ChannelOpenHandle, Msg, Response, Server as _, Session};
use russh::{Channel, ChannelId, Sig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStdin, Command};

fn event(text: impl AsRef<str>) {
    println!("EVENT {}", text.as_ref());
}

#[derive(Clone)]
struct Server {
    password: String,
    authorized: Arc<Vec<PublicKey>>,
    modes: Modes,
}

impl russh::server::Server for Server {
    type Handler = Connection;

    fn new_client(&mut self, peer: Option<std::net::SocketAddr>) -> Connection {
        event(format!(
            "connection from {}",
            peer.map_or("?".into(), |p| p.ip().to_string())
        ));
        Connection {
            server: self.clone(),
            children: HashMap::new(),
        }
    }
}

struct Child {
    stdin: Option<ChildStdin>,
    pid: Option<u32>,
}

struct Connection {
    server: Server,
    children: HashMap<ChannelId, Child>,
}

fn signal_number(sig: &Sig) -> String {
    match sig {
        Sig::Custom(name) => name.clone(),
        other => format!("{other:?}"),
    }
}

fn sig_of(number: i32) -> Sig {
    match number {
        1 => Sig::HUP,
        2 => Sig::INT,
        6 => Sig::ABRT,
        9 => Sig::KILL,
        11 => Sig::SEGV,
        15 => Sig::TERM,
        other => Sig::Custom(other.to_string()),
    }
}

impl russh::server::Handler for Connection {
    type Error = russh::Error;

    async fn auth_succeeded(&mut self, session: &mut Session) -> Result<(), Self::Error> {
        if let Some(code) = self.server.modes.disconnect_on_auth {
            event(format!("disconnecting after authentication with reason {code}"));
            session.disconnect(russh::Disconnect::try_from(code)?, "spike disconnect", "")?;
        }
        Ok(())
    }

    async fn auth_none(&mut self, user: &str) -> Result<Auth, Self::Error> {
        event(format!("auth none user={user}"));
        Ok(Auth::reject())
    }

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        let ok = password == self.server.password;
        event(format!("auth password user={user} accepted={ok}"));
        Ok(if ok { Auth::Accept } else { Auth::reject() })
    }

    async fn auth_publickey_offered(&mut self, user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        let ok = self.server.authorized.iter().any(|k| k.key_data() == key.key_data());
        event(format!(
            "auth publickey offered user={user} algorithm={} accepted={ok}",
            key.algorithm()
        ));
        Ok(if ok { Auth::Accept } else { Auth::reject() })
    }

    async fn auth_publickey(&mut self, user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        let ok = self.server.authorized.iter().any(|k| k.key_data() == key.key_data());
        event(format!("auth publickey signed user={user} accepted={ok}"));
        Ok(if ok { Auth::Accept } else { Auth::reject() })
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        user: &str,
        _submethods: &str,
        response: Option<Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        match response {
            None => {
                event(format!("auth keyboard-interactive prompt user={user}"));
                Ok(Auth::Partial {
                    name: "".into(),
                    instructions: "".into(),
                    prompts: vec![("Password: ".into(), false)].into(),
                })
            }
            Some(mut answers) => {
                let answer = answers.next();
                let ok = answer
                    .as_ref()
                    .is_some_and(|a| a.as_ref() == self.server.password.as_bytes());
                let length = answer.as_ref().map_or(0, |a| a.len());
                event(format!(
                    "auth keyboard-interactive answer user={user} length={length} accepted={ok}"
                ));
                Ok(if ok { Auth::Accept } else { Auth::reject() })
            }
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        event(format!("channel open session {:?}", channel.id()));
        reply.accept().await;
        Ok(())
    }

    async fn env_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        _value: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        event(format!("env request {channel:?} {name} (rejected)"));
        session.channel_failure(channel)?;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        event(format!("pty request {channel:?} (rejected)"));
        session.channel_failure(channel)?;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let command = String::from_utf8_lossy(data).to_string();
        event(format!("exec {channel:?} {command}"));
        if self.server.modes.reject_exec {
            event("rejecting the exec request");
            session.channel_failure(channel)?;
            return Ok(());
        }
        if let Some(code) = self.server.modes.disconnect_on_exec {
            event(format!("disconnecting with reason {code}"));
            session.disconnect(russh::Disconnect::try_from(code)?, "spike disconnect", "")?;
            return Ok(());
        }
        let child = Command::new("sh")
            .arg("-c")
            .arg(&command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match child {
            Ok(child) => child,
            Err(e) => {
                event(format!("spawn failed: {e}"));
                session.channel_failure(channel)?;
                return Ok(());
            }
        };
        session.channel_success(channel)?;
        let handle = session.handle();
        let (mut stdout, mut stderr) = (child.stdout.take(), child.stderr.take());
        self.children.insert(
            channel,
            Child {
                stdin: child.stdin.take(),
                pid: child.id(),
            },
        );
        let withhold_close = self.server.modes.hang_on_close;
        tokio::spawn(async move {
            let out_handle = handle.clone();
            let out = tokio::spawn(async move {
                let mut buf = vec![0u8; 32768];
                while let Some(stream) = stdout.as_mut() {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if out_handle.data(channel, buf[..n].to_vec()).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            });
            let err_handle = handle.clone();
            let err = tokio::spawn(async move {
                let mut buf = vec![0u8; 32768];
                while let Some(stream) = stderr.as_mut() {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if err_handle.extended_data(channel, 1, buf[..n].to_vec()).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            });
            let status = child.wait().await;
            let _ = tokio::join!(out, err);
            // OpenSSH's order: exit-status or exit-signal, EOF, close
            match status {
                Ok(status) => {
                    use std::os::unix::process::ExitStatusExt;
                    if let Some(code) = status.code() {
                        event(format!("exit {channel:?} status={code}"));
                        let _ = handle.exit_status_request(channel, code as u32).await;
                    } else if let Some(signal) = status.signal() {
                        event(format!("exit {channel:?} signal={signal}"));
                        let _ = handle
                            .exit_signal_request(channel, sig_of(signal), false, String::new(), String::new())
                            .await;
                    }
                }
                Err(e) => event(format!("wait failed: {e}")),
            }
            let _ = handle.eof(channel).await;
            if !withhold_close {
                let _ = handle.close(channel).await;
            }
        });
        Ok(())
    }

    async fn data(&mut self, channel: ChannelId, data: &[u8], _session: &mut Session) -> Result<(), Self::Error> {
        if let Some(stdin) = self.children.get_mut(&channel).and_then(|c| c.stdin.as_mut()) {
            let _ = stdin.write_all(data).await;
        }
        Ok(())
    }

    async fn channel_eof(&mut self, channel: ChannelId, _session: &mut Session) -> Result<(), Self::Error> {
        event(format!("eof from client {channel:?}"));
        if let Some(child) = self.children.get_mut(&channel) {
            child.stdin = None;
        }
        Ok(())
    }

    async fn signal(&mut self, channel: ChannelId, signal: Sig, _session: &mut Session) -> Result<(), Self::Error> {
        let name = signal_number(&signal);
        event(format!("signal {channel:?} {name}"));
        if let Some(pid) = self.children.get(&channel).and_then(|c| c.pid) {
            let _ = Command::new("kill")
                .arg(format!("-{name}"))
                .arg(pid.to_string())
                .status()
                .await;
        }
        Ok(())
    }

    async fn channel_close(&mut self, channel: ChannelId, _session: &mut Session) -> Result<(), Self::Error> {
        event(format!("close from client {channel:?}"));
        self.children.remove(&channel);
        if self.server.modes.hang_on_close {
            // russh queues the reply CLOSE before calling this handler; never returning keeps it from being flushed
            event("hanging instead of completing the close");
            std::future::pending::<()>().await;
        }
        Ok(())
    }
}

/// Misbehaviours of the fake, for the rows that a real sshd cannot provoke
#[derive(Clone, Debug, Default)]
pub struct Modes {
    /// Never complete a CLOSE from the client (T15)
    pub hang_on_close: bool,
    /// Answer every exec request with failure
    pub reject_exec: bool,
    /// Send DISCONNECT with this reason code instead of running the command
    pub disconnect_on_exec: Option<u32>,
    /// Send DISCONNECT with this reason code once authentication succeeded
    pub disconnect_on_auth: Option<u32>,
}

impl Modes {
    pub fn parse(args: &[String]) -> anyhow::Result<Modes> {
        let mut modes = Modes::default();
        for arg in args {
            if arg == "--hang-on-close" {
                modes.hang_on_close = true;
            } else if arg == "--reject-exec" {
                modes.reject_exec = true;
            } else if let Some(code) = arg.strip_prefix("--disconnect-on-exec=") {
                modes.disconnect_on_exec = Some(code.parse()?);
            } else if let Some(code) = arg.strip_prefix("--disconnect-on-auth=") {
                modes.disconnect_on_auth = Some(code.parse()?);
            }
        }
        Ok(modes)
    }
}

pub async fn serve(port: u16, authorized_keys: &Path, host_key: &Path, modes: Modes) -> anyhow::Result<()> {
    let authorized = std::fs::read_to_string(authorized_keys)?
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .filter_map(|key| keys::parse_public_key_base64(key).ok())
        .collect::<Vec<_>>();
    let config = russh::server::Config {
        keys: vec![keys::load_secret_key(host_key, None)?],
        auth_rejection_time: std::time::Duration::from_millis(100),
        auth_rejection_time_initial: Some(std::time::Duration::from_millis(0)),
        ..Default::default()
    };
    let mut server = Server {
        // "fake-password" is a fixture value for the local fake, not a credential
        password: std::env::var("FAKE_PASSWORD").unwrap_or_else(|_| "fake-password".into()),
        authorized: Arc::new(authorized),
        modes,
    };
    event(format!("listening on 127.0.0.1:{port} {:?}", server.modes));
    server.run_on_address(Arc::new(config), ("127.0.0.1", port)).await?;
    Ok(())
}
