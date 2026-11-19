use std::{
    io,
    sync::{Condvar, Mutex, MutexGuard},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Target {
    Group(i32),
    Client(i32),
}

impl Target {
    fn address(self) -> i32 {
        match self {
            Self::Group(id) => -id,
            Self::Client(id) => id,
        }
    }

    /// Requires a registered, unreaped leader, except when polling for disappearance.
    #[allow(unsafe_code)]
    pub(super) fn signal(self, signal: i32) -> io::Result<()> {
        // SAFETY: a positive PID or a negative private PGID. Signal 0 has no side effects.
        if unsafe { libc::kill(self.address(), signal) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[derive(Default)]
struct State {
    sealed: Option<Instant>,
    in_flight: usize,
    targets: Vec<Target>,
}

pub(super) struct Registry {
    state: Mutex<State>,
    completed: Condvar,
}

impl Registry {
    pub(super) const fn new() -> Self {
        Self {
            state: Mutex::new(State {
                sealed: None,
                in_flight: 0,
                targets: Vec::new(),
            }),
            completed: Condvar::new(),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub(super) fn admit(&self) -> io::Result<Launching<'_>> {
        let mut state = self.state();
        if state.sealed.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "process admission is sealed",
            ));
        }
        state.in_flight += 1;
        Ok(Launching(self))
    }

    pub(super) fn signal(&self, target: Target, signal: i32) -> io::Result<()> {
        let _state = self.state();
        match target.signal(signal) {
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(()),
            result => result,
        }
    }

    /// Serialize unregistration with the emergency sweep, before the leader can be reaped.
    pub(super) fn retire(&self, target: Target, signal: Option<i32>, exited: bool) -> io::Result<()> {
        let mut state = self.state();
        if let Some(signal) = signal {
            match target.signal(signal) {
                Err(error)
                    if error.raw_os_error() != Some(libc::ESRCH)
                        // macOS can report EPERM when only the unreaped leader remains. The caller
                        // still needs to check that the group disappears after reaping that leader.
                        && !(cfg!(target_os = "macos")
                            && exited
                            && matches!(target, Target::Group(_))
                            && error.raw_os_error() == Some(libc::EPERM)) =>
                {
                    return Err(error);
                }
                _ => {}
            }
        }
        if let Some(index) = state.targets.iter().position(|&registered| registered == target) {
            state.targets.swap_remove(index);
        }
        Ok(())
    }

    pub(super) fn sweep(&self) -> usize {
        let mut state = self.state();
        let deadline = *state
            .sealed
            .get_or_insert_with(|| Instant::now() + Duration::from_millis(200));
        self.completed.notify_all();
        for &target in &state.targets {
            let _ = target.signal(libc::SIGKILL);
        }
        let (state, _) = self
            .completed
            .wait_timeout_while(state, deadline.saturating_duration_since(Instant::now()), |state| {
                state.in_flight != 0
            })
            .unwrap_or_else(|error| error.into_inner());
        state.in_flight
    }
}

pub(super) struct Launching<'a>(&'a Registry);

impl Launching<'_> {
    pub(super) fn register(&self, target: Target) {
        let mut state = self.0.state();
        state.targets.push(target);
        if state.sealed.is_some() {
            let _ = target.signal(libc::SIGKILL);
        }
    }
}

impl Drop for Launching<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state();
        state.in_flight -= 1;
        if state.sealed.is_some() && state.in_flight == 0 {
            self.0.completed.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::process::{CommandExt, ExitStatusExt},
        process::Command,
    };

    #[test]
    fn sweep_kills_registered_and_late_groups_and_seals_admission() -> io::Result<()> {
        let registry = Registry::new();
        let registered = registry.admit()?;
        let mut first = Command::new("/bin/sleep").arg("120").process_group(0).spawn()?;
        let first_target = Target::Group(first.id() as i32);
        registered.register(first_target);
        drop(registered);

        let late = registry.admit()?;
        let mut second = Command::new("/bin/sleep").arg("120").process_group(0).spawn()?;
        let second_target = Target::Group(second.id() as i32);
        std::thread::scope(|scope| {
            scope.spawn(|| registry.sweep());
            let state = registry.state();
            let state = registry
                .completed
                .wait_timeout_while(state, Duration::from_secs(60), |state| state.sealed.is_none())
                .unwrap_or_else(|error| error.into_inner())
                .0;
            assert!(state.sealed.is_some());
            drop(state);
            late.register(second_target);
            drop(late);
        });

        assert!(registry.admit().is_err());
        for (target, child) in [(first_target, &mut first), (second_target, &mut second)] {
            let end = Instant::now() + Duration::from_secs(60);
            let status = loop {
                if let Some(status) = super::super::observe_exit(child.id())? {
                    assert!(!status);
                    registry.retire(target, None, true)?;
                    break child.try_wait()?;
                }
                if Instant::now() >= end {
                    let _ = child.kill();
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "sweep did not kill group"));
                }
                std::thread::sleep(Duration::from_millis(5));
            };
            assert_eq!(status.and_then(|status| status.signal()), Some(libc::SIGKILL));
        }
        Ok(())
    }
}
