//! Port of vendor/vendor-android/src/test/kotlin/com/malinskiy/marathon/android/model/RotationTest.kt
//! and core/src/main/kotlin/com/malinskiy/marathon/device/screenshot/ScreenCapturer.kt (oracle witnesses).
use marathon_error::FailureKind;
mod report_probe_support;
use image::{Rgba, RgbaImage};
use jcompat::{jtime::duration_of, logging::MarathonLogging};
use marathon_core::{
    error::*,
    executor::{
        CoreExecutor, Signal,
        hooks::{ClockMode, RuntimeHooks, Tape},
        shared::{failure, requested},
    },
    runtime::*,
    screenshot::*,
};
use report_probe_support::*;
use std::{
    io::Cursor,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
#[test]
fn test_value() {
    for (r, v) in Rotation::VALUES.into_iter().zip([0, 2, 3, 1]) {
        assert_eq!(r.value(), v)
    }
}
#[test]
fn test_of() {
    for (v, r) in [
        Some(Rotation::Rotation0),
        Some(Rotation::Rotation90),
        Some(Rotation::Rotation180),
        Some(Rotation::Rotation270),
        None,
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(Rotation::of(v as i32), r)
    }
}
fn image(w: u32, h: u32, gradient: bool) -> RgbaFrame {
    RgbaImage::from_fn(w, h, |x, y| {
        if gradient {
            Rgba([
                (x * 255 / (w.max(2) - 1)) as u8,
                (y * 255 / (h.max(2) - 1)) as u8,
                0,
                255,
            ])
        } else {
            [
                Rgba([255, 0, 0, 255]),
                Rgba([0, 255, 0, 255]),
                Rgba([0, 0, 255, 255]),
                Rgba([255, 255, 255, 255]),
            ][((x + y * w) % 4) as usize]
        }
    })
}
type DecodedFrame = (u16, u16, u16, gif::DisposalMethod, Option<u8>, Vec<u8>);
fn decoded(bytes: &[u8]) -> Vec<DecodedFrame> {
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    let mut reader = checked(options.read_info(bytes));
    let mut result = Vec::new();
    while let Some(frame) = checked(reader.read_next_frame()) {
        result.push((
            frame.width,
            frame.height,
            frame.delay,
            frame.dispose,
            frame.transparent,
            frame.buffer.to_vec(),
        ))
    }
    result
}
#[test]
fn jvm_all_rotations_swapped_and_square_frames() {
    for rotation in Rotation::VALUES {
        for (w, h) in [(2, 3), (3, 2), (2, 2)] {
            let mut transform = FrameTransform::default();
            let mut writer = checked(GifSequenceWriter::new(Vec::new(), 0, true));
            for (w, h) in [(w, h), (h, w)] {
                checked(writer.write_to_sequence(&checked(transform.transform(image(w, h, false), rotation, 2, 3))))
            }
            let actual = checked(writer.finish());
            let expected = checked(std::fs::read(fixture(&format!(
                "capture-{}-{w}-{h}.gif",
                rotation.value()
            ))));
            assert_eq!(decoded(&actual), decoded(&expected), "rotation {rotation:?}, {w}x{h}");
            assert_eq!(actual.windows(8).filter(|s| *s == b"NETSCAPE").count(), 2);
        }
    }
}
#[test]
fn gif_metadata_zero_frames_and_invalid_delays_match_jvm() {
    assert_eq!(
        checked(checked(GifSequenceWriter::new(Vec::new(), 10, true)).finish()),
        checked(std::fs::read(fixture("gif-zero.gif")))
    );
    for delay in [0, 19] {
        let mut writer = checked(GifSequenceWriter::new(Vec::new(), delay, false));
        for _ in 0..2 {
            checked(writer.write_to_sequence(&image(2, 3, false)))
        }
        let bytes = checked(writer.finish());
        let expected = checked(std::fs::read(fixture(&format!("gif-{delay}.gif"))));
        assert_eq!(decoded(&bytes), decoded(&expected));
        // Negative witness: one changed delay in a graphic control block must not match.
        let mut changed = bytes.clone();
        let gce = changed.windows(3).position(|w| w == b"\x21\xf9\x04").unwrap();
        changed[gce + 4] ^= 1;
        assert_ne!(decoded(&changed), decoded(&expected));
        assert!(!bytes.windows(7).any(|w| w == b"Created"));
    }
    for delay in [-10, 655360] {
        let err = GifSequenceWriter::new(Vec::new(), delay, false).err().unwrap();
        let expected = oracle(&format!("gif-{delay}"));
        let class = expected["error"].as_str().unwrap().split_once(": ").unwrap().0;
        assert_eq!(err.failure().kind, marathon_test_support::recorded_errors::kind(class));
    }
}
#[test]
fn gradient_resize_geometry_and_measured_pixels() {
    for (w, h) in [(7, 11), (11, 7), (2, 3), (3, 2)] {
        let actual = checked(FrameTransform::default().transform(image(w, h, true), Rotation::Rotation0, 5, 8));
        let expected = checked(image::open(fixture(&format!("resize-{w}-{h}.png")))).to_rgba8();
        assert_eq!(actual.dimensions(), expected.dimensions());
        let max = actual
            .as_raw()
            .iter()
            .zip(expected.as_raw())
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        println!("resize-{w}-{h} max channel difference {max}");
        assert_eq!(max, 0);
    }
}
#[test]
fn resize_ties_follow_awt_fixed_point_sampling() {
    let row = oracle("resize-ties");
    let actual = checked(FrameTransform::default().transform(image(2, 4, false), Rotation::Rotation0, 1, 2));
    assert_eq!(
        (actual.width(), actual.height()),
        (
            row["width"].as_u64().unwrap() as u32,
            row["height"].as_u64().unwrap() as u32
        )
    );
    let argb: Vec<i64> = actual
        .pixels()
        .map(|p| i64::from(u32::from_be_bytes([p[3], p[0], p[1], p[2]]) as i32))
        .collect();
    assert_eq!(serde_json::json!(argb), row["pixels"]);
}
struct Script {
    frames: Mutex<std::collections::VecDeque<VendorResult<Option<RgbaFrame>>>>,
    calls: AtomicUsize,
    orientation: Rotation,
}
impl Screenshottable for Script {
    fn orientation(&self) -> VendorResult<Rotation> {
        assert_ne!(std::thread::current().name(), Some("marathon-core"));
        Ok(self.orientation)
    }
    fn screenshot<'a>(
        &'a self,
        timeout: jcompat::jtime::Duration,
        _: Arc<dyn CancellationSignal>,
    ) -> VendorFuture<'a, Option<RgbaFrame>> {
        Box::pin(async move {
            assert_eq!(timeout.to_millis(), Some(7));
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.frames
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(VendorError::Cancelled(requested("done"))))
        })
    }
}
struct Clock {
    delays: Mutex<Vec<i64>>,
}
impl VendorClock for Clock {
    fn delay(&self, millis: i64, cancel: Arc<dyn CancellationSignal>) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            self.delays.lock().unwrap().push(millis);
            if let Some(reason) = cancel.reason() {
                Err(VendorError::Cancelled(reason))
            } else {
                Ok(())
            }
        })
    }
    fn with_timeout<'a>(
        &'a self,
        _: Arc<dyn CancellationSignal>,
        _: i64,
        _: VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, ()> {
        panic!("unused")
    }
    fn with_timeout_or_null<'a>(
        &'a self,
        _: Arc<dyn CancellationSignal>,
        _: i64,
        _: VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, Option<()>> {
        panic!("unused")
    }
}
#[test]
fn capture_timeout_io_null_continue_fatal_and_cancellation_join_cleanup() {
    for terminal in [
        VendorError::Cancelled(requested("done")),
        VendorError::Other(failure(FailureKind::IllegalState, "fatal")),
        VendorError::Cancelled(Cancellation::Timeout {
            scope: 7,
            detail: failure(FailureKind::TimeoutCancelled, "runtime"),
        }),
    ] {
        let temp = Temp::new();
        let root = temp.path("");
        let expected = terminal.clone();
        let clock = Arc::new(Clock {
            delays: Mutex::new(Vec::new()),
        });
        let log = Arc::new(Mutex::new(Vec::new()));
        let (c, l) = (clock.clone(), log.clone());
        checked(CoreExecutor::run(move |core| {
            Box::pin(async move {
                let source = Arc::new(Script {
                    frames: Mutex::new(
                        [
                            Ok(None),
                            Err(VendorError::Other(failure(FailureKind::Timeout, "source"))),
                            Err(VendorError::Other(failure(FailureKind::Io, "source"))),
                            Ok(Some(image(2, 3, false))),
                            Err(terminal),
                        ]
                        .into(),
                    ),
                    calls: AtomicUsize::new(0),
                    orientation: Rotation::Rotation0,
                });
                let hooks = RuntimeHooks::taped(
                    Tape {
                        wall: ClockMode::Step { start: 0, step: 5 },
                        ..Tape::default()
                    },
                    &core.clock(),
                );
                let settings = CaptureSettings {
                    device: device(),
                    pool: marathon_core::model::DevicePoolId { name: "pool".into() },
                    batch_id: "batch".into(),
                    delay: duration_of(0, 19_000_000).unwrap(),
                    height: 3,
                    width: 2,
                    timeout: duration_of(0, 7_000_000).unwrap(),
                };
                let services = CaptureServices {
                    io: core.vendor(),
                    clock: c,
                    wall: hooks,
                    files: output(core.vendor(), root),
                    log: Arc::new(move |_, level, message, _| {
                        l.lock().unwrap().push((level, message.to_owned()));
                    }),
                };
                let mut cap = ScreenCapturer::new(settings, services, source, &mut MarathonLogging::default());
                assert_eq!(cap.start(&test("capture"), Signal::new()).await, Err(expected));
                assert_eq!(cap.captures_taken(), 1);
                Ok(())
            })
        }));
        assert_eq!(*clock.delays.lock().unwrap(), [14, 14, 14, 14]);
        assert_eq!(log.lock().unwrap().len(), 1);
        let file = temp.0.join("screenshot/pool/serial-#1/pkg.Clazz#capture-batch.gif");
        assert_eq!(decoded(&checked(std::fs::read(file))).len(), 1);
    }
}
fn capturer(
    core: &CoreExecutor,
    root: std::path::PathBuf,
    frames: Vec<VendorResult<Option<RgbaFrame>>>,
    delay: jcompat::jtime::Duration,
) -> ScreenCapturer {
    let source = Arc::new(Script {
        frames: Mutex::new(frames.into()),
        calls: AtomicUsize::new(0),
        orientation: Rotation::Rotation0,
    });
    let hooks = RuntimeHooks::taped(
        Tape {
            wall: ClockMode::Step { start: 0, step: 5 },
            ..Tape::default()
        },
        &core.clock(),
    );
    let settings = CaptureSettings {
        device: device(),
        pool: marathon_core::model::DevicePoolId { name: "pool".into() },
        batch_id: "batch".into(),
        delay,
        height: 3,
        width: 2,
        timeout: duration_of(0, 7_000_000).unwrap(),
    };
    let services = CaptureServices {
        io: core.vendor(),
        clock: Arc::new(Clock {
            delays: Mutex::new(Vec::new()),
        }),
        wall: hooks,
        files: output(core.vendor(), root),
        log: Arc::new(|_, _, _, _| {}),
    };
    ScreenCapturer::new(settings, services, source, &mut MarathonLogging::default())
}
#[test]
fn start_owned_runs_as_a_spawned_static_task() {
    let temp = Temp::new();
    let root = temp.path("");
    checked(CoreExecutor::run(move |core| {
        Box::pin(async move {
            let cap = Arc::new(capturer(
                &core,
                root,
                vec![Ok(Some(image(2, 3, false))), Ok(Some(image(2, 3, false)))],
                duration_of(0, 19_000_000).unwrap(),
            ));
            let task = core
                .vendor()
                .spawn(cap.clone().start_owned(&test("owned"), Signal::new()));
            // The script cancels after its frames; join returns once cleanup finished.
            assert_eq!(task.join().await, Err(VendorError::Cancelled(requested("done"))));
            assert_eq!(cap.captures_taken(), 2);
            Ok(())
        })
    }));
    let file = temp.0.join("screenshot/pool/serial-#1/pkg.Clazz#owned-batch.gif");
    assert_eq!(decoded(&checked(std::fs::read(file))).len(), 2);
}
#[test]
fn overflowing_delay_fails_after_creating_the_file() {
    let temp = Temp::new();
    let root = temp.path("");
    checked(CoreExecutor::run(move |core| {
        Box::pin(async move {
            let mut cap = capturer(&core, root, vec![], duration_of(i64::MAX, 0).unwrap());
            let error = cap.start(&test("overflow"), Signal::new()).await.err().unwrap();
            assert_eq!(
                error.failure().to_string().to_owned(),
                "arithmetic failure: long overflow"
            );
            Ok(())
        })
    }));
    // FileImageOutputStream exists before Kotlin's lazy delayMillis throws.
    let file = temp.0.join("screenshot/pool/serial-#1/pkg.Clazz#overflow-batch.gif");
    assert_eq!(checked(std::fs::read(file)), b"");
}
#[test]
fn explicit_finish_reports_stream_failure() {
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("write"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    assert!(checked(GifSequenceWriter::new(Broken, 0, true)).finish().is_err());
    let mut writer = checked(GifSequenceWriter::new(Broken, 0, true));
    assert!(writer.write_to_sequence(&image(2, 3, false)).is_err());
    let mut writer = checked(GifSequenceWriter::new(Cursor::new(Vec::new()), 0, true));
    checked(writer.write_to_sequence(&image(2, 3, false)));
    let mut stream = checked(writer.finish());
    std::io::Write::write_all(&mut stream, b"still open").unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn persistent_dimensions_and_gradient_palette_measurement() {
    let mut transform = FrameTransform::default();
    for (i, (w, h)) in [(2, 3), (3, 2)].into_iter().enumerate() {
        let actual = checked(transform.transform(image(w, h, false), Rotation::Rotation0, 2, 3));
        let mut writer = checked(GifSequenceWriter::new(Vec::new(), 0, true));
        checked(writer.write_to_sequence(&actual));
        assert_eq!(
            decoded(&checked(writer.finish())),
            decoded(&checked(std::fs::read(fixture(&format!("reuse{i}.gif")))))
        );
    }
    for name in ["alpha", "gradient"] {
        let input = checked(image::open(fixture(&format!("{name}-input.png")))).to_rgba8();
        let mut writer = checked(GifSequenceWriter::new(Vec::new(), 19, true));
        checked(writer.write_to_sequence(&input));
        let bytes = checked(writer.finish());
        let actual = decoded(&bytes);
        let expected = decoded(&checked(std::fs::read(fixture(&format!("{name}.gif")))));
        assert_eq!(
            (&actual[0].0, &actual[0].1, &actual[0].2, &actual[0].3, &actual[0].4),
            (
                &expected[0].0,
                &expected[0].1,
                &expected[0].2,
                &expected[0].3,
                &expected[0].4
            )
        );
        // Absolute differences over every RGBA byte (alpha included), as approved for P1.
        let measure = |a: &[u8], b: &[u8]| {
            let differences: Vec<_> = a.iter().zip(b).map(|(a, b)| a.abs_diff(*b)).collect();
            let max = *differences.iter().max().unwrap();
            (
                max,
                differences.iter().map(|v| f64::from(*v)).sum::<f64>() / differences.len() as f64,
            )
        };
        let (max, mean) = measure(&actual[0].5, &expected[0].5);
        let (rust_max, rust_mean) = measure(&actual[0].5, input.as_raw());
        let (jvm_max, jvm_mean) = measure(&expected[0].5, input.as_raw());
        println!(
            "{name}: rust/jvm max={max} mean={mean}; rust/input max={rust_max} mean={rust_mean}; jvm/input max={jvm_max} mean={jvm_mean}"
        );
        if name == "alpha" {
            assert_eq!(actual, expected)
        } else {
            assert!(max <= 80 && mean <= 9.0);
            // Pin the measurement documented in the findings: NeuQuant is further from the
            // input than ImageIO's palette on this 1,024-colour witness.
            // Alpha never differs here, so the RGB-only means are these times 4/3.
            assert_eq!((max, rust_max, jvm_max), (77, 75, 5));
            assert_eq!((mean, rust_mean, jvm_mean), (8.312744140625, 8.064697265625, 2.078125));
        }
    }
}

#[derive(Default)]
struct CaptureGate {
    armed: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    released: Mutex<bool>,
    condition: std::sync::Condvar,
    cleaned: std::sync::atomic::AtomicBool,
}
struct GateRuntime {
    inner: Arc<dyn VendorRuntime>,
    gate: Arc<CaptureGate>,
}
impl VendorRuntime for GateRuntime {
    fn spawn_cancellable(&self, body: VendorTaskBody) -> Arc<dyn VendorTask> {
        self.inner.spawn_cancellable(body)
    }
    fn spawn(&self, body: VendorFuture<'static, ()>) -> Arc<dyn VendorTask> {
        self.inner.spawn(body)
    }
    fn spawn_blocking(&self, body: Box<dyn FnOnce() -> VendorResult<()> + Send>) -> Arc<dyn VendorTask> {
        let gate = self.gate.clone();
        let wait = gate.armed.swap(false, Ordering::AcqRel);
        self.inner.spawn_blocking(Box::new(move || {
            if wait {
                let mut released = gate.released.lock().unwrap();
                gate.entered.notify_one();
                while !*released {
                    released = gate.condition.wait(released).unwrap()
                }
            }
            body()
        }))
    }
}
struct GateSource {
    mode: &'static str,
    gate: Arc<CaptureGate>,
}
impl Screenshottable for GateSource {
    fn orientation(&self) -> VendorResult<Rotation> {
        Ok(Rotation::Rotation0)
    }
    fn screenshot<'a>(
        &'a self,
        _: jcompat::jtime::Duration,
        cancellation: Arc<dyn CancellationSignal>,
    ) -> VendorFuture<'a, Option<RgbaFrame>> {
        Box::pin(async move {
            if self.mode == "capture" {
                self.gate.entered.notify_one();
                let reason = cancellation.cancelled().await;
                self.gate.cleaned.store(true, Ordering::Release);
                return Err(VendorError::Cancelled(reason));
            }
            if self.mode == "write" {
                self.gate.armed.store(true, Ordering::Release)
            }
            Ok(Some(image(2, 3, false)))
        })
    }
}
struct GateClock {
    gate: Arc<CaptureGate>,
}
impl VendorClock for GateClock {
    fn delay(&self, _: i64, cancellation: Arc<dyn CancellationSignal>) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            self.gate.entered.notify_one();
            Err(VendorError::Cancelled(cancellation.cancelled().await))
        })
    }
    fn with_timeout<'a>(
        &'a self,
        _: Arc<dyn CancellationSignal>,
        _: i64,
        _: VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, ()> {
        panic!("unused")
    }
    fn with_timeout_or_null<'a>(
        &'a self,
        _: Arc<dyn CancellationSignal>,
        _: i64,
        _: VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, Option<()>> {
        panic!("unused")
    }
}
/// The JVM `cancel-*` rows' observable outcome: captures, completion and source cleanup.
fn cancel_matches(row: &serde_json::Value, captures: i32, completed: bool, cleanup: bool) -> bool {
    row["captures"] == captures && row["completed"] == completed && row["sourceCleanup"] == cleanup
}
#[test]
fn cancellation_during_capture_write_and_delay_waits_for_cleanup() {
    for mode in ["capture", "write", "delay"] {
        let observed = Arc::new(Mutex::new(None));
        let seen = observed.clone();
        let temp = Temp::new();
        let root = temp.path("");
        checked(CoreExecutor::run(move |core| {
            Box::pin(async move {
                let gate = Arc::new(CaptureGate::default());
                let signal = Signal::new();
                let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let runtime = Arc::new(GateRuntime {
                    inner: core.vendor(),
                    gate: gate.clone(),
                });
                let source = Arc::new(GateSource {
                    mode,
                    gate: gate.clone(),
                });
                let hooks = RuntimeHooks::taped(
                    Tape {
                        wall: ClockMode::Frozen(0),
                        ..Tape::default()
                    },
                    &core.clock(),
                );
                let services = CaptureServices {
                    io: runtime,
                    clock: Arc::new(GateClock { gate: gate.clone() }),
                    wall: hooks,
                    files: output(core.vendor(), root),
                    log: Arc::new(|_, _, _, _| {}),
                };
                let settings = CaptureSettings {
                    device: device(),
                    pool: marathon_core::model::DevicePoolId { name: "pool".into() },
                    batch_id: "batch".into(),
                    delay: duration_of(1, 0).unwrap(),
                    height: 3,
                    width: 2,
                    timeout: duration_of(0, 7_000_000).unwrap(),
                };
                let mut cap = ScreenCapturer::new(settings, services, source, &mut MarathonLogging::default());
                let (s, d) = (signal.clone(), done.clone());
                let task = core.vendor().spawn(Box::pin(async move {
                    let result = cap.start(&test("cancel"), s).await;
                    assert_eq!(result, Err(VendorError::Cancelled(requested("cancel"))));
                    assert_eq!(cap.captures_taken(), if mode == "capture" { 0 } else { 1 });
                    *seen.lock().unwrap() = Some(cap.captures_taken());
                    d.store(true, Ordering::Release);
                    Ok(())
                }));
                gate.entered.notified().await;
                signal.cancel(requested("cancel"));
                if mode == "write" {
                    assert!(!done.load(Ordering::Acquire));
                    *gate.released.lock().unwrap() = true;
                    gate.condition.notify_one();
                }
                checked(task.join().await);
                assert!(done.load(Ordering::Acquire));
                let cleaned = gate.cleaned.load(Ordering::Acquire);
                assert_eq!(cleaned, mode == "capture");
                if mode != "write" {
                    let row = oracle(&format!("cancel-{mode}"));
                    let captures = observed.lock().unwrap().unwrap_or(-1);
                    assert!(cancel_matches(&row, captures, true, cleaned), "{mode}");
                    // Negative witnesses: an omitted cleanup or an unfinished job must not match.
                    assert!(!cancel_matches(&row, captures, true, !cleaned));
                    assert!(!cancel_matches(&row, captures, false, cleaned));
                }
                Ok(())
            })
        }));
        let bytes = checked(std::fs::read(
            temp.0.join("screenshot/pool/serial-#1/pkg.Clazz#cancel-batch.gif"),
        ));
        if mode == "capture" {
            assert_eq!(bytes, [0x3b])
        } else {
            assert_eq!(decoded(&bytes).len(), 1)
        }
    }
}
