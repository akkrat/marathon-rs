//! Faults shared by the Apple setup and preparation witnesses for U212.

use marathon_core::{
    analytics::{MappingTracker, TrackerInternalAdapter},
    executor::{
        RuntimeClock,
        hooks::{RuntimeHooks, Tape},
    },
    report::{DevicePreparingEvent, DeviceProviderPreparingEvent},
    runtime::VendorFuture,
    time::ReplayHooks,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Clone, Copy)]
pub enum TimingFault {
    FirstRead,
    LastRead,
    TrackerPanic,
}

impl TimingFault {
    pub fn instrument(self, label: &str) -> (Arc<RuntimeHooks>, Arc<MappingTracker>, Arc<PreparationEvents>) {
        let values = match self {
            Self::FirstRead => vec![],
            Self::LastRead => vec![10],
            Self::TrackerPanic => vec![10, 20],
        };
        let mut tape = Tape::default();
        tape.reads.insert(label.into(), values);
        let hooks = RuntimeHooks::taped(tape, &RuntimeClock::virtual_clock());
        let events = Arc::new(PreparationEvents {
            calls: AtomicUsize::new(0),
            panic: matches!(self, Self::TrackerPanic),
            provider: label == "core.Track.trackProviderDevicePreparing",
        });
        let tracker = Arc::new(MappingTracker::new(hooks.clone(), events.clone()));
        (hooks, tracker, events)
    }

    pub fn verify(self, hooks: &dyn ReplayHooks, label: &str, events: &PreparationEvents) {
        match self {
            Self::FirstRead | Self::LastRead => {
                assert_eq!(events.calls.load(Ordering::SeqCst), 0);
                let error = hooks
                    .verify_consumed()
                    .expect_err("clock fault must still fail replay validation");
                assert_eq!(error.label, label);
                assert_eq!(error.message, "read tape exhausted");
            }
            Self::TrackerPanic => {
                assert_eq!(events.calls.load(Ordering::SeqCst), 1);
                assert_eq!(hooks.verify_consumed(), Ok(()));
            }
        }
    }
}

pub struct PreparationEvents {
    calls: AtomicUsize,
    panic: bool,
    provider: bool,
}

impl PreparationEvents {
    fn track(&self, provider: bool) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            if provider == self.provider {
                self.calls.fetch_add(1, Ordering::SeqCst);
                assert!(!self.panic, "preparation tracker panicked");
            }
            Ok(())
        })
    }
}

impl TrackerInternalAdapter for PreparationEvents {
    fn device_preparing<'a>(&'a self, _: &'a DevicePreparingEvent) -> VendorFuture<'a, ()> {
        self.track(false)
    }

    fn device_provider_preparing<'a>(&'a self, _: &'a DeviceProviderPreparingEvent) -> VendorFuture<'a, ()> {
        self.track(true)
    }
}
