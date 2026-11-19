//! Check group membership while the unreaped leader still pins its ID.

use std::io;

#[cfg(target_os = "macos")]
pub(super) fn has_other_members(leader: i32) -> io::Result<bool> {
    has_other_matching(leader, |member| member.pbi_status != libc::SZOMB)
}

#[cfg(target_os = "macos")]
pub(super) fn all_members_exiting(leader: i32) -> io::Result<bool> {
    // Group kill can miss processes in exit() before they become waitable.
    // Their exit still needs to be observed during cleanup.
    const PROC_FLAG_INEXIT: u32 = 4; // sys/proc_info.h
    let exiting =
        |member: &libc::proc_bsdinfo| member.pbi_status == libc::SZOMB || member.pbi_flags & PROC_FLAG_INEXIT != 0;
    Ok(member_info(leader)?.as_ref().is_some_and(exiting) && !has_other_matching(leader, |member| !exiting(member))?)
}

#[cfg(target_os = "macos")]
fn has_other_matching(leader: i32, active: impl Fn(&libc::proc_bsdinfo) -> bool) -> io::Result<bool> {
    let mut pids = vec![0; 8];
    loop {
        let count = list_members(leader, &mut pids)?;
        if count < pids.len() {
            pids.truncate(count);
            break;
        }
        pids.resize(pids.len() * 2, 0);
    }
    if pids.is_empty() || pids == [leader] {
        return Ok(false);
    }
    for pid in pids.iter_mut().filter(|pid| **pid != leader) {
        match member_info(*pid)? {
            Some(member) if active(&member) => return Ok(true),
            Some(_) => {}
            // If this PID is still in the group on the next scan, we lack proof.
            None => *pid = 0,
        }
    }
    // A member could have forked between the first list and the status lookup.
    // Recheck membership after inspecting those peers and refuse any new PID.
    let mut latest = vec![0; pids.len() + 1];
    let count = list_members(leader, &mut latest)?;
    pids.sort_unstable();
    Ok(latest[..count].iter().any(|pid| pids.binary_search(pid).is_err()))
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn list_members(group: i32, pids: &mut [i32]) -> io::Result<usize> {
    let size = i32::try_from(std::mem::size_of_val(pids)).map_err(io::Error::other)?;
    // SAFETY: libproc writes at most size bytes into the supplied PID buffer.
    // It can report an error as zero results with errno set.
    unsafe {
        let errno = libc::__error();
        *errno = 0;
        let count = libc::proc_listpgrppids(group, pids.as_mut_ptr().cast(), size);
        if count < 0 || (count == 0 && *errno != 0) {
            return Err(io::Error::last_os_error());
        }
        if count as usize > pids.len() {
            return Err(io::Error::other("invalid local process group size"));
        }
        Ok(count as usize)
    }
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn member_info(pid: i32) -> io::Result<Option<libc::proc_bsdinfo>> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    // SAFETY: the buffer is correctly sized for PROC_PIDTBSDINFO. A nonzero
    // argument includes zombies. Only a full record is read.
    let count = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 1, info.as_mut_ptr().cast(), size) };
    if count == size {
        return Ok(Some(unsafe { info.assume_init() }));
    }
    if count == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(None);
        }
        return Err(error);
    }
    Err(io::Error::other("incomplete local process status"))
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub(super) fn has_other_members(leader: i32) -> io::Result<bool> {
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry.file_name().to_str().and_then(|name| name.parse::<i32>().ok()) else {
            continue;
        };
        if pid == leader {
            continue;
        }
        // SAFETY: a read-only lookup for a PID seen in this procfs. Processes
        // disappearing or changing group during the scan cannot be signalled here.
        let group = unsafe { libc::getpgid(pid) };
        if group == leader {
            return Ok(true);
        }
        if group < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
    }
    Ok(false)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::{Command, observe_exit};
    use std::{process::Stdio, time::Duration};
    use tokio::io::{AsyncBufReadExt, BufReader};

    #[tokio::test]
    async fn a_zombie_only_group_confirms_cleanup_d96() -> io::Result<()> {
        tokio::time::timeout(Duration::from_secs(60), async {
            let mut owned = Command::new("/bin/sh")
                .args(["-c", "read line; exit 31"])
                .stdin(Stdio::piped())
                .spawn()?;
            let leader = owned.id().ok_or_else(|| io::Error::other("missing group leader"))?;

            // Retain the zombies in the original group using a parent in a
            // separate group. One member stays live until the test resumes it.
            let mut keeper = Command::new("/usr/bin/perl")
                .args([
                    "-e",
                    r#"
use POSIX qw(_exit setpgid raise SIGSTOP);
my @children;
for my $index (0..8) {
    my $child = fork // die "fork: $!";
    if ($child) { push @children, $child; next; }
    setpgid(0, $ARGV[0]) == 0 or die "join group: $!";
    $| = 1;
    print "$index $$\n";
    raise(SIGSTOP) if $index == 0;
    _exit(0);
}
scalar <STDIN>;
waitpid($_, 0) for @children;
"#,
                    &leader.to_string(),
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()?;
            let keeper_input = keeper.take_stdin();
            let stdout = keeper
                .take_stdout()?
                .ok_or_else(|| io::Error::other("missing keeper output"))?;
            let mut lines = BufReader::new(stdout).lines();
            let mut members = Vec::with_capacity(9);
            let mut stopped = 0;
            for _ in 0..9 {
                let line = lines
                    .next_line()
                    .await?
                    .ok_or_else(|| io::Error::other("missing member"))?;
                let (index, pid) = line.split_once(' ').ok_or_else(|| io::Error::other("invalid member"))?;
                let pid = pid.parse::<i32>().map_err(io::Error::other)?;
                if index == "0" {
                    stopped = pid;
                }
                members.push(pid);
            }
            assert!(stopped > 0);
            while member_status(stopped)? != Some(libc::SSTOP) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            drop(owned.take_stdin());
            while observe_exit(leader)?.is_none() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert!(has_other_members(leader as i32)?);
            assert!(!all_members_exiting(leader as i32)?);
            resume(stopped)?;
            for pid in members {
                while member_status(pid)? != Some(libc::SZOMB) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }
            owned.terminate()?;
            owned.start_kill()?;
            assert_eq!(owned.wait().await?.code(), Some(31));
            assert!(owned.cleanup_confirmed());
            assert_eq!(member_status(stopped)?, Some(libc::SZOMB));
            drop(keeper_input);
            assert!(keeper.wait().await?.success());
            Ok(())
        })
        .await?
    }

    fn member_status(pid: i32) -> io::Result<Option<u32>> {
        Ok(member_info(pid)?.map(|member| member.pbi_status))
    }

    #[allow(unsafe_code)]
    fn resume(pid: i32) -> io::Result<()> {
        // SAFETY: the fixture's parent keeps this child pinned until teardown.
        if unsafe { libc::kill(pid, libc::SIGCONT) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}
