//! The CLI's process-death backstop.

#![allow(unsafe_code)]

use std::{
    io::{self, Write},
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
};

const SLOTS: usize = 4096;
const RECORD_SIZE: usize = 8;

pub(super) struct Reaper {
    pipe: std::fs::File,
    free: Vec<usize>,
}

impl Reaper {
    /// The caller must still be single-threaded.
    pub(super) unsafe fn start() -> io::Result<Self> {
        // SAFETY: the CLI is still single-threaded and hasn't launched children.
        // An inherited SIG_IGN would let the kernel reap a leader before removal.
        unsafe {
            let mut child: libc::sigaction = std::mem::zeroed();
            child.sa_sigaction = libc::SIG_DFL;
            libc::sigemptyset(&mut child.sa_mask);
            if libc::sigaction(libc::SIGCHLD, &child, std::ptr::null_mut()) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        let (reader, writer) = pipe()?;
        // The helper reports on this pipe once it leads its own process group. The parent
        // waits for that report instead of setting the group itself: on macOS the parent's
        // setpgid can fail with EPERM while the child is changing its own group.
        let (ready_reader, ready_writer) = pipe()?;
        // Enumerate in the parent, where directory I/O and allocation are safe. The
        // directory's own descriptor will already be closed when the helper forks.
        #[cfg(target_os = "linux")]
        let directory = "/proc/self/fd";
        #[cfg(target_os = "macos")]
        let directory = "/dev/fd";
        let inherited = std::fs::read_dir(directory)?
            .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().parse::<RawFd>().ok()))
            .collect::<io::Result<Vec<_>>>()?;
        let reader_fd = reader.as_raw_fd();
        let ready_fd = ready_writer.as_raw_fd();
        let inherited = inherited
            .into_iter()
            .flatten()
            .filter(|&fd| fd != reader_fd && fd != ready_fd)
            .collect::<Vec<_>>();
        let signals = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGPIPE];
        // SAFETY: no other thread can change the fd table or receive these signals while
        // we fork. The child uses only stack memory and async-signal-safe calls.
        unsafe {
            let mut blocked: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut blocked);
            for signal in signals {
                libc::sigaddset(&mut blocked, signal);
            }
            let mut previous: libc::sigset_t = std::mem::zeroed();
            if libc::sigprocmask(libc::SIG_BLOCK, &blocked, &mut previous) != 0 {
                return Err(io::Error::last_os_error());
            }
            let child = libc::fork();
            if child == 0 {
                run(reader_fd, ready_fd, &inherited, signals);
            }
            let start_error = if child < 0 {
                Some(io::Error::last_os_error())
            } else {
                // Only the helper keeps the write end: an exit before its report reads as EOF.
                drop(ready_writer);
                let report = await_report(ready_reader.as_raw_fd());
                if report.is_err() {
                    // The helper is exiting, or the report could not be read: end it either
                    // way (it cannot ignore KILL) and don't leave a zombie behind.
                    libc::kill(child, libc::SIGKILL);
                    let mut status = 0;
                    libc::waitpid(child, &mut status, 0);
                }
                report.err()
            };
            libc::sigprocmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut());
            if let Some(error) = start_error {
                return Err(error);
            }
        }
        drop(reader);
        Ok(Self::with_writer(writer))
    }

    fn with_writer(writer: OwnedFd) -> Self {
        Self {
            pipe: writer.into(),
            free: (0..SLOTS).rev().collect(),
        }
    }

    pub(super) fn add(&mut self, address: i32) -> io::Result<usize> {
        let slot = self
            .free
            .last()
            .copied()
            .ok_or_else(|| io::Error::new(io::ErrorKind::WouldBlock, "process reaper registry is full"))?;
        self.write(slot, address)?;
        self.free.pop();
        Ok(slot)
    }

    pub(super) fn remove(&mut self, slot: usize) -> io::Result<()> {
        self.write(slot, 0)?;
        self.free.push(slot);
        Ok(())
    }

    fn write(&mut self, slot: usize, address: i32) -> io::Result<()> {
        let mut record = [0; RECORD_SIZE];
        record[..4].copy_from_slice(&(slot as u32).to_ne_bytes());
        record[4..].copy_from_slice(&address.to_ne_bytes());
        // A record is smaller than PIPE_BUF, so a nonblocking write succeeds
        // atomically or fails before writing any bytes.
        self.pipe.write_all(&record)
    }
}

fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    // SAFETY: pipe initializes two new descriptors on success. OwnedFd closes them
    // on setup errors. This is called before any other thread can fork.
    unsafe {
        let mut fds = [0; 2];
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return Err(io::Error::last_os_error());
        }
        let reader = OwnedFd::from_raw_fd(fds[0]);
        let writer = OwnedFd::from_raw_fd(fds[1]);
        for fd in fds {
            if libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) == -1 {
                return Err(io::Error::last_os_error());
            }
        }
        if libc::fcntl(fds[1], libc::F_SETFL, libc::O_NONBLOCK) == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok((reader, writer))
    }
}

/// The helper's startup report: it leads its own process group and ignores the signals.
const READY: u8 = 0;
const NO_GROUP: u8 = 1;
const NO_SIGNALS: u8 = 2;

unsafe fn run(reader: RawFd, ready: RawFd, inherited: &[RawFd], signals: [i32; 4]) -> ! {
    // SAFETY: this is the single-threaded fork child. Do not unwind, allocate, or
    // touch Rust runtime state. These signals stay blocked as well as ignored.
    unsafe {
        // Out of reach of group-directed KILL before the parent goes on.
        if libc::setpgid(0, 0) != 0 {
            fail(ready, NO_GROUP);
        }
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = libc::SIG_IGN;
        libc::sigemptyset(&mut action.sa_mask);
        for signal in signals {
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                fail(ready, NO_SIGNALS);
            }
        }
        for &fd in inherited {
            libc::close(fd);
        }
        libc::write(ready, (&READY as *const u8).cast(), 1);
        libc::close(ready);
        drain(reader, |address| {
            libc::kill(address, libc::SIGKILL);
        });
        libc::_exit(0);
    }
}

/// Reports a startup failure from the fork child and exits it.
unsafe fn fail(ready: RawFd, report: u8) -> ! {
    // SAFETY: write and _exit are async-signal-safe; the byte lives on the stack.
    unsafe {
        libc::write(ready, (&report as *const u8).cast(), 1);
        libc::_exit(1)
    }
}

/// Waits for the helper's startup report. EOF means it exited before reporting.
fn await_report(ready: RawFd) -> io::Result<()> {
    let mut report = 0u8;
    loop {
        // SAFETY: the parent owns this fd; the buffer is one byte on the stack.
        let len = unsafe { libc::read(ready, (&mut report as *mut u8).cast(), 1) };
        if len < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        return match (len, report) {
            (1, READY) => Ok(()),
            (1, NO_GROUP) => Err(io::Error::other(
                "the process reaper could not lead its own process group",
            )),
            (1, _) => Err(io::Error::other(
                "the process reaper could not ignore termination signals",
            )),
            _ => Err(io::Error::other("the process reaper exited during startup")),
        };
    }
}

fn drain(reader: RawFd, mut kill: impl FnMut(i32)) {
    let mut targets = [0; SLOTS];
    let mut record = [0u8; RECORD_SIZE];
    let mut used = 0;
    loop {
        // SAFETY: the reader owns this fd and the unused tail of the buffer.
        let tail = &mut record[used..];
        let len = unsafe { libc::read(reader, tail.as_mut_ptr().cast(), tail.len()) };
        if len == 0 {
            break;
        }
        if len < 0 {
            if read_interrupted() {
                continue;
            }
            return;
        }
        used += len as usize;
        if used != RECORD_SIZE {
            continue;
        }
        used = 0;
        let slot = u32::from_ne_bytes([record[0], record[1], record[2], record[3]]) as usize;
        let address = i32::from_ne_bytes([record[4], record[5], record[6], record[7]]);
        if let Some(target) = targets.get_mut(slot) {
            *target = address;
        }
    }
    for address in targets {
        if address != 0 {
            kill(address);
        }
    }
}

fn read_interrupted() -> bool {
    // SAFETY: the fork child is reading its own thread-local errno.
    #[cfg(target_os = "linux")]
    let errno = unsafe { *libc::__errno_location() };
    #[cfg(target_os = "macos")]
    let errno = unsafe { *libc::__error() };
    errno == libc::EINTR
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_pipe_preserves_the_registration_and_removal_slot() -> io::Result<()> {
        let (read, write) = pipe()?;
        let mut reaper = Reaper::with_writer(write);
        let slot = reaper.add(-123)?;
        let size = reaper.free.len();
        while reaper.write(10, 0).is_ok() {}
        assert_eq!(reaper.remove(slot).unwrap_err().kind(), io::ErrorKind::WouldBlock);
        assert_eq!(reaper.free.len(), size);
        drop(reaper);
        let mut killed = Vec::new();
        drain(read.as_raw_fd(), |address| killed.push(address));
        assert_eq!(killed, [-123]);
        Ok(())
    }

    #[test]
    fn reaper_ignores_removed_groups_even_when_a_slot_is_reused() -> io::Result<()> {
        let (read, write) = pipe()?;
        let mut reaper = Reaper::with_writer(write);
        let retired = reaper.add(-123)?;
        let other = reaper.add(-456)?;
        reaper.remove(retired)?;
        assert_eq!(reaper.add(-789)?, retired);
        reaper.remove(other)?;
        drop(reaper);
        let mut killed = Vec::new();
        drain(read.as_raw_fd(), |address| killed.push(address));
        assert_eq!(killed, [-789]);
        Ok(())
    }
}
