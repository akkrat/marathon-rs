//! U199: reconfiguring a console must not disable callsites that an earlier console registered.
//!
//! Its own test binary on purpose: the failure needs this console to be the only registered dispatcher, which other
//! tests running in parallel in a shared binary would hide.

use std::{
    io::Write,
    sync::{Arc, Mutex},
};

use marathon_cli::application::CliOutput;
use marathon_logging::{OutputTarget, Vendor};

#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl Write for Buffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// One console: configured from this thread (outside its dispatcher's scope, as `run_cli` configures it), then the
/// same callsite emits inside its scope. Returns what reached its stdout.
fn console_run() -> String {
    let stdout = Buffer::default();
    let output = CliOutput::new(
        Box::new(stdout.clone()),
        Box::new(Buffer::default()),
        OutputTarget::Pipe,
        Box::new(|_| String::from("12:34:56.789")),
    )
    .unwrap_or_else(|error| panic!("test console: {error}"));
    output.logging().lock().unwrap().configure(true, Vendor::Android);
    tracing::dispatcher::with_default(&output.console().tracing_dispatch(), || {
        tracing::error!(target: "marathon_cli::application", "crash line");
    });
    drop(output);
    String::from_utf8(stdout.0.lock().unwrap().clone()).unwrap()
}

#[test]
fn a_later_console_keeps_a_callsite_an_earlier_one_registered_u199() {
    // The first console registers the callsite; the second is configured while it is the only dispatcher.
    assert!(console_run().contains("crash line"), "first console");
    assert!(console_run().contains("crash line"), "second console lost the line");
}
