use super::reaper::Reaper;
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
    pub(super) fn address(self) -> i32 {
        match self {
            Self::Group(id) => -id,
            Self::Client(id) => id,
        }
    }

    /// Requires an unreaped leader.
    #[allow(unsafe_code)]
    pub(super) fn signal(self, signal: i32) -> io::Result<()> {
        // SAFETY: a positive PID or a negative private PGID.
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
    targets: Vec<(Target, Option<usize>)>,
    reaper: Option<Reaper>,
}

pub(super) struct Registry {
    state: Mutex<State>,
    completed: Condvar,
}

#[cfg(test)]
type RetireWitness = (Target, fn(u32));

#[cfg(test)]
thread_local! {
    pub(super) static RETIRE_WITNESS: std::cell::Cell<Option<RetireWitness>> = const { std::cell::Cell::new(None) };
}

impl Registry {
    pub(super) const fn new() -> Self {
        Self {
            state: Mutex::new(State {
                sealed: None,
                in_flight: 0,
                targets: Vec::new(),
                reaper: None,
            }),
            completed: Condvar::new(),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub(super) fn set_reaper(&self, reaper: Reaper) {
        self.state().reaper = Some(reaper);
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

    /// Serialize removal with the sweep and publish it before the leader can be reaped.
    pub(super) fn retire(&self, target: Target) -> io::Result<()> {
        let mut state = self.state();
        let mut lost = None;
        if let Some(index) = state.targets.iter().position(|&(registered, _)| registered == target) {
            if let Some(slot) = state.targets[index].1
                && let Some(reaper) = &mut state.reaper
                && let Err(error) = reaper.remove(slot)
            {
                if error.kind() != io::ErrorKind::BrokenPipe {
                    return Err(error);
                }
                lost = state.reaper.take();
            }
            state.targets.swap_remove(index);
        }
        drop(state);
        if lost.is_some() {
            warn_reaper_lost();
        }
        #[cfg(test)]
        RETIRE_WITNESS.with(|witness| {
            if let Some((watched, check)) = witness.get()
                && watched == target
            {
                witness.set(None);
                check(target.address().unsigned_abs());
            }
        });
        Ok(())
    }

    pub(super) fn sweep(&self) -> usize {
        let mut state = self.state();
        let deadline = *state
            .sealed
            .get_or_insert_with(|| Instant::now() + Duration::from_millis(200));
        self.completed.notify_all();
        for &(target, _) in &state.targets {
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
    pub(super) fn register(&self, target: Target) -> io::Result<()> {
        let mut state = self.0.state();
        let mut slot = None;
        let mut lost = None;
        let result = match state.reaper.as_mut().map(|reaper| reaper.add(target.address())) {
            Some(Ok(assigned)) => {
                slot = Some(assigned);
                Ok(())
            }
            Some(Err(error)) if error.kind() == io::ErrorKind::BrokenPipe => {
                lost = state.reaper.take();
                Ok(())
            }
            Some(Err(error)) => Err(error),
            None => Ok(()),
        };
        if state.sealed.is_some() || result.is_err() {
            let _ = target.signal(libc::SIGKILL);
        }
        state.targets.push((target, slot));
        drop(state);
        if lost.is_some() {
            warn_reaper_lost();
        }
        result
    }
}

fn warn_reaper_lost() {
    tracing::warn!(target: "marathon_process", "Process reaper stopped; the controller will still sweep its registered processes");
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
        registered.register(first_target)?;
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
            late.register(second_target).ok();
            drop(late);
        });

        assert!(registry.admit().is_err());
        for (target, child) in [(first_target, &mut first), (second_target, &mut second)] {
            let end = Instant::now() + Duration::from_secs(60);
            let status = loop {
                if let Some(status) = super::super::observe_exit(child.id())? {
                    assert!(!status.success());
                    registry.retire(target)?;
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
