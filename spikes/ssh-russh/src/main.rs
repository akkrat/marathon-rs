//! WP 0.3a spike: a minimal russh executor driven through the T01-T19 matrix of docs/research/ssh.md, and a fake-worker
//! server. Throwaway code; see docs/research/ssh-spike-results.md.
//!
//! ssh-russh-spike <case> [args]       the target comes from SPIKE_* variables, as for spikes/ssh-sshj (run-case.sh)
//! ssh-russh-spike serve <port> <authorized_keys> <host_key> [--hang-on-close] [--reject-exec]
//!     [--disconnect-on-exec=<reason>] [--disconnect-on-auth=<reason>]

mod cases;
mod cases_round2;
mod exec;
mod lines;
mod quote;
mod server;

use std::path::PathBuf;
use std::sync::atomic::AtomicU32;
use std::time::Duration;

use exec::{Auth, Target};

fn env(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn target() -> anyhow::Result<Target> {
    let auth = env("SPIKE_AUTH", "");
    let auth = if auth == "agent" {
        Auth::Agent
    } else if let Some(password) = auth.strip_prefix("password:") {
        Auth::Password(password.into())
    } else if let Some(key) = auth.strip_prefix("key:") {
        Auth::Key(key.into())
    } else {
        anyhow::bail!("bad SPIKE_AUTH");
    };
    Ok(Target {
        host: env("SPIKE_HOST", ""),
        port: env("SPIKE_PORT", "22").parse()?,
        user: env("SPIKE_USER", ""),
        auth,
        known_hosts: Some(env("SPIKE_KNOWN_HOSTS", ""))
            .filter(|p| !p.is_empty())
            .map(PathBuf::from),
        keep_alive: Duration::from_millis(env("SPIKE_KA_MS", "15000").parse()?),
        keep_alive_max: env("SPIKE_KA_MAX", "4").parse()?,
        attempts: env("SPIKE_ATTEMPTS", "4").parse()?,
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let case = args.first().map(String::as_str).unwrap_or_default();
    if case == "serve" {
        let port = args.get(1).map_or(Ok(22230), |p| p.parse())?;
        let authorized = PathBuf::from(args.get(2).cloned().unwrap_or_default());
        let host_key = PathBuf::from(args.get(3).cloned().unwrap_or_default());
        return server::serve(
            port,
            &authorized,
            &host_key,
            server::Modes::parse(args.get(4..).unwrap_or(&[]))?,
        )
        .await;
    }
    if case == "shell-command" {
        println!("wire: {}", quote::shell_command(&args[1..]));
        return Ok(());
    }
    let t = target()?;
    if case == "connect" {
        cases::connect_case(&t).await;
        return Ok(());
    }
    let executor = exec::connect(&t, &AtomicU32::new(0)).await?;
    match case {
        "t01" => cases::t01(&t, &executor).await,
        "t02" => cases::t02(&t, &executor).await,
        "t03" => cases::t03(&t, &executor).await,
        "t04" => cases::t04(&t, &executor).await,
        "t05" => cases::t05(&t, &executor).await,
        "t06" => cases::t06(&t, &executor).await,
        "t07" => cases::t07(&t, &executor).await,
        "t08" => cases::t08(&t, &executor).await,
        "t09" => cases::t09(&t, &executor, args.get(1).map_or(Ok(20000), |a| a.parse())?).await,
        "t11" => cases::t11(&t, &executor).await,
        "t14" => cases::t14(&t, &executor).await,
        "t15h" => {
            cases::t15h(&t, &executor).await;
            std::process::exit(0);
        }
        "t15t" => cases::t15t(&t, &executor).await,
        "t21" => cases_round2::t21(&t, &executor).await,
        "t22" => cases_round2::t22(&t, &executor).await,
        "t23" => cases_round2::t23(&t, &executor).await,
        "t24" => cases_round2::t24(&t, &executor).await,
        "t25" => cases_round2::t25(&t, &executor).await,
        "t15" => {
            cases::t15(&t, &executor).await;
            // The session task of a hung server never finishes; leave without a graceful disconnect
            std::process::exit(0);
        }
        "t16" => {
            // t16 closes its connections itself
            cases::t16(&t, executor).await;
            return Ok(());
        }
        "t18" => cases::t18(&t, &executor).await,
        other => anyhow::bail!("unknown case {other}"),
    }
    executor.close().await;
    Ok(())
}
