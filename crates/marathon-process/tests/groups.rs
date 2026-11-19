use marathon_process::Command;
use std::{io, os::unix::process::ExitStatusExt, process::Stdio, time::Duration};
use tokio::io::{AsyncBufReadExt, BufReader};

#[allow(unsafe_code)]
fn child_exited(pid: u32) -> io::Result<bool> {
    // SAFETY: waitid initializes the supplied record. WNOWAIT leaves the launcher in charge of reaping.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid,
            &mut info,
            libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { info.si_pid() } != 0)
}

#[allow(unsafe_code)]
fn signal(pid: i32, signal: i32) -> io::Result<()> {
    // SAFETY: the tests control both child PIDs and only probe identities after cleanup.
    if unsafe { libc::kill(pid, signal) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[tokio::test]
async fn group_disappears_when_its_leader_exits_first() -> io::Result<()> {
    tokio::time::timeout(Duration::from_secs(60), async {
        let mut unrelated = Command::new("/bin/sleep").arg("120").spawn()?;
        let mut owned = Command::new("/bin/sh")
            .args(["-c", "/bin/sleep 120 & printf '%s\\n' \"$!\""])
            .stdout(Stdio::piped())
            .spawn()?;
        let leader = owned.id().ok_or_else(|| io::Error::other("missing leader"))?;
        let stdout = owned.take_stdout()?.ok_or_else(|| io::Error::other("missing stdout"))?;
        let mut stdout = BufReader::new(stdout);
        let mut pid = String::new();
        stdout.read_line(&mut pid).await?;
        let descendant = pid.trim().parse::<i32>().map_err(io::Error::other)?;
        while !child_exited(leader)? {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        signal(descendant, 0)?;
        let result = owned.wait().await;
        if result.is_err() {
            let _ = signal(descendant, libc::SIGKILL);
        }
        assert!(result?.success());
        assert_eq!(
            signal(-(leader as i32), 0).unwrap_err().raw_os_error(),
            Some(libc::ESRCH)
        );
        let unrelated_pid = unrelated
            .id()
            .ok_or_else(|| io::Error::other("missing unrelated process"))?;
        signal(unrelated_pid as i32, 0)?;
        unrelated.kill().await
    })
    .await?
}

#[tokio::test]
async fn adb_client_cleanup_preserves_a_server_during_startup() -> io::Result<()> {
    tokio::time::timeout(Duration::from_secs(60), async {
        let mut client = Command::new("/bin/sh")
            .args(["-c", "/bin/sleep 120 >/dev/null & printf '%s\\n' \"$!\"; wait"])
            .stdout(Stdio::piped())
            .spawn_adb_client()?;
        let stdout = client
            .take_stdout()?
            .ok_or_else(|| io::Error::other("missing stdout"))?;
        let mut stdout = BufReader::new(stdout);
        let mut pid = String::new();
        stdout.read_line(&mut pid).await?;
        let server = pid.trim().parse::<i32>().map_err(io::Error::other)?;
        let result = client.kill().await;
        let server_remained = signal(server, 0).is_ok();
        // Teardown the fake daemon without relying on the launcher's client policy.
        let _ = signal(server, libc::SIGKILL);
        result?;
        assert_eq!(client.wait().await?.signal(), Some(libc::SIGKILL));
        assert!(server_remained);
        Ok(())
    })
    .await?
}

#[tokio::test]
#[ignore = "manual native spawn overhead comparison"]
async fn measure_1000_launches() -> io::Result<()> {
    let direct = std::time::Instant::now();
    for _ in 0..1000 {
        assert!(std::process::Command::new("/usr/bin/true").status()?.success());
    }
    let direct = direct.elapsed();
    let owned = std::time::Instant::now();
    for _ in 0..1000 {
        assert!(Command::new("/usr/bin/true").spawn()?.wait().await?.success());
    }
    eprintln!("direct {direct:.3?}, registered {:.3?}", owned.elapsed());
    Ok(())
}
