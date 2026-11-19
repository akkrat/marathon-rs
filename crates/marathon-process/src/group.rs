//! Check group membership while the unreaped leader still pins its ID.

use std::io;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub(super) fn has_other_members(leader: i32) -> io::Result<bool> {
    let mut pids = [0i32; 2];
    // SAFETY: the buffer holds two pids. One PID besides the exited leader is
    // enough to know that the group cannot be retired. The query can report an
    // error as zero results with errno set.
    unsafe {
        let errno = libc::__error();
        *errno = 0;
        let count = libc::proc_listpgrppids(leader, pids.as_mut_ptr().cast(), std::mem::size_of_val(&pids) as i32);
        if count < 0 || (count == 0 && *errno != 0) {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(pids.into_iter().any(|pid| pid != 0 && pid != leader))
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
