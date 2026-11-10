//! Port of vendor/vendor-android/src/test/kotlin/com/malinskiy/marathon/android/model/RotationTest.kt
//! and core/src/main/kotlin/com/malinskiy/marathon/device/screenshot/ScreenCapturer.kt (oracle witnesses).
use marathon_logging::LogSettings;

use marathon_error::FailureKind;
mod report_probe_support;
use image::{Rgba, RgbaImage};
use jcompat::jtime::duration_of;
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
use sha2::{Digest, Sha256};
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
            if w != h {
                assert_eq!(decoded(&actual), decoded(&expected), "rotation {rotation:?}, {w}x{h}");
            } else {
                // C64's extra quarter-turn stays; D63 changes the 2→3 resize ties.
                let [r, g, b, w] = [
                    [255, 0, 0, 255],
                    [0, 255, 0, 255],
                    [0, 0, 255, 255],
                    [255, 255, 255, 255],
                ];
                let [tl, tr, bl, br] =
                    [[r, g, b, w], [b, r, w, g], [w, b, g, r], [g, w, r, b]][((rotation.value() + 1) % 4) as usize];
                let pixels: Vec<u8> = [tl, tr, tr, bl, br, br, bl, br, br].into_iter().flatten().collect();
                let frames = decoded(&actual);
                assert_eq!(frames.len(), 2);
                for frame in frames {
                    assert_eq!(
                        (frame.0, frame.1, frame.2, frame.3, frame.4),
                        (3, 3, 0, gif::DisposalMethod::Any, None)
                    );
                    assert_eq!(frame.5, pixels);
                }
            }
            assert_eq!(actual.windows(8).filter(|s| *s == b"NETSCAPE").count(), 1);
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
        assert_eq!(bytes.windows(8).filter(|s| *s == b"NETSCAPE").count(), 1);
        assert!(
            bytes
                .windows(19)
                .any(|w| w == b"\x21\xff\x0bNETSCAPE2.0\x03\x01\x01\x00\x00")
        );
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
    for (w, h, xs, ys) in [
        (7, 11, vec![0, 2, 3, 4, 6], vec![0, 2, 3, 4, 6, 7, 8, 10]),
        (11, 7, vec![0, 2, 3, 4, 6, 7, 8, 10], vec![0, 2, 3, 4, 6]),
        (2, 3, vec![0, 0, 1, 1, 1], vec![0, 0, 0, 1, 1, 2, 2, 2]),
        (3, 2, vec![0, 0, 0, 1, 1, 2, 2, 2], vec![0, 0, 1, 1, 1]),
    ] {
        let input = image(w, h, true);
        let actual = checked(FrameTransform::default().transform(input.clone(), Rotation::Rotation0, 5, 8));
        assert_eq!(actual.dimensions(), (xs.len() as u32, ys.len() as u32));
        for (y, &sy) in ys.iter().enumerate() {
            for (x, &sx) in xs.iter().enumerate() {
                assert_eq!(actual.get_pixel(x as u32, y as u32), input.get_pixel(sx, sy));
            }
        }
    }
}
#[test]
fn resize_ties_follow_native_nearest_sampling_d63() {
    let input = RgbaImage::from_fn(2, 4, |x, y| Rgba([x as u8, y as u8, 0, 255]));
    let reduced = checked(FrameTransform::default().transform(input.clone(), Rotation::Rotation0, 1, 2));
    assert_eq!(reduced.into_raw(), [1, 1, 0, 255, 1, 3, 0, 255]);
    let expanded = checked(FrameTransform::default().transform(input.clone(), Rotation::Rotation0, 3, 6));
    assert_eq!(expanded.dimensions(), (3, 6));
    assert_eq!(
        expanded.rows().next().unwrap().map(|p| p[0]).collect::<Vec<_>>(),
        [0, 1, 1]
    );
    let same = checked(FrameTransform::default().transform(input.clone(), Rotation::Rotation0, 2, 4));
    assert_eq!(same, input);
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
                let mut cap = ScreenCapturer::new(settings, services, source, &mut LogSettings::default());
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
    ScreenCapturer::new(settings, services, source, &mut LogSettings::default())
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
    let name = "gradient";
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
    assert!(max <= 80 && mean <= 9.0);
    assert_eq!((max, rust_max, jvm_max), (77, 75, 5));
    assert_eq!((mean, rust_mean, jvm_mean), (8.312744140625, 8.064697265625, 2.078125));
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
                let mut cap = ScreenCapturer::new(settings, services, source, &mut LogSettings::default());
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

#[test]
fn opaque_gif_bytes_are_pinned_d63() {
    let input = checked(image::open(fixture("gradient-input.png"))).to_rgba8();
    let mut writer = checked(GifSequenceWriter::new(Vec::new(), 19, true));
    checked(writer.write_to_sequence(&input));
    let bytes = checked(writer.finish());
    assert_eq!(bytes.len(), 1470);
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        "99041382c522e495f239b4c36cad29d340d36fab74e5ecb4de4c6188360cba83"
    );
    let input = RgbaImage::from_fn(2, 3, |x, y| Rgba([(x * 255) as u8, (y * 127) as u8, 0, 255]));
    let mut writer = checked(GifSequenceWriter::new(Vec::new(), 19, true));
    checked(writer.write_to_sequence(&input));
    let bytes = checked(writer.finish());
    assert_eq!(bytes.len(), 88);
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        "92447fc0a7c30b135c647e1defb811c011bbe8d5e3afe694913b47d183db00e0"
    );
}

#[test]
fn gif_partial_alpha_uses_binary_transparency_d63() {
    let input = checked(image::open(fixture("alpha-input.png"))).to_rgba8();
    let mut writer = checked(GifSequenceWriter::new(Vec::new(), 19, true));
    checked(writer.write_to_sequence(&input));
    let bytes = checked(writer.finish());
    let frames = decoded(&bytes);
    assert_eq!(frames.len(), 1);
    assert_eq!(
        (frames[0].0, frames[0].1, frames[0].2, frames[0].3),
        (4, 1, 1, gif::DisposalMethod::Any)
    );
    for (expected, actual) in input.pixels().zip(frames[0].5.as_chunks::<4>().0.iter()) {
        if expected[3] == 0 {
            assert_eq!(*actual, [0, 0, 0, 0]);
        } else {
            assert_eq!(*actual, [expected[0], expected[1], expected[2], 255]);
        }
    }
}

#[test]
fn quantized_gif_above_256_colors_preserves_the_alpha_mask_d63() {
    for variant in 0..3 {
        let input = RgbaImage::from_fn(64, 64, |x, y| {
            let alpha = if (x + 3 * y) % 7 == 0 {
                0
            } else if x % 2 == 0 {
                128
            } else {
                255
            };
            match variant {
                0 => Rgba([(x * 4) as u8, (y * 4) as u8, ((x + y) * 2) as u8, alpha]),
                1 => Rgba([x as u8, y as u8, ((x + y) % 32) as u8, alpha]),
                _ => Rgba([
                    (255 - x * 4) as u8,
                    (255 - y * 4) as u8,
                    ((x * 13 + y * 7) % 256) as u8,
                    alpha,
                ]),
            }
        });
        let colors: std::collections::HashSet<_> = input
            .pixels()
            .filter(|p| p[3] != 0)
            .map(|p| [p[0], p[1], p[2]])
            .collect();
        assert!(colors.len() > 256);
        let mut writer = checked(GifSequenceWriter::new(Vec::new(), 120, false));
        let mut next = input.clone();
        for pixel in next.pixels_mut() {
            pixel[3] = if pixel[3] == 0 { 255 } else { 0 };
        }
        let next_colors: std::collections::HashSet<_> = next
            .pixels()
            .filter(|p| p[3] != 0)
            .map(|p| [p[0], p[1], p[2]])
            .collect();
        assert!(next_colors.len() > 256);
        checked(writer.write_to_sequence(&input));
        checked(writer.write_to_sequence(&next));
        let bytes = checked(writer.finish());
        let frames = decoded(&bytes);
        assert_eq!(frames.len(), 2);
        assert_eq!(bytes.windows(8).filter(|s| *s == b"NETSCAPE").count(), 1);
        for (frame, source) in frames.into_iter().zip([&input, &next]) {
            assert_eq!(
                (frame.0, frame.1, frame.2, frame.3),
                (64, 64, 12, gif::DisposalMethod::Any)
            );
            for (expected, actual) in source.pixels().zip(frame.5.as_chunks::<4>().0.iter()) {
                assert_eq!(actual[3], if expected[3] == 0 { 0 } else { 255 }, "variant {variant}");
            }
        }
    }
}
