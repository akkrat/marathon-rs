//! Port of core/src/main/kotlin/com/malinskiy/marathon/device/screenshot/ScreenCapturer.kt,
//! GifSequenceWriter.kt, Rotation.kt and Screenshottable.kt.
//!
//! GifSequenceWriter: created by Elliot Kroo, 2009-04-25.
//! Licensed under Creative Commons Attribution 3.0 Unported:
//! http://creativecommons.org/licenses/by/3.0/ .

use crate::{artifact_io::*, error::*, executor::shared::failure, model::*, output::*, runtime::*, time::WallClock};
use image::{RgbaImage, imageops};
use jcompat::{
    jtime::Duration,
    logging::{Level, Logger, MarathonLogging},
};
use std::{
    io::Write,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI32, Ordering},
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rotation {
    Rotation0,
    Rotation180,
    Rotation270,
    Rotation90,
}
impl Rotation {
    pub const VALUES: [Self; 4] = [Self::Rotation0, Self::Rotation180, Self::Rotation270, Self::Rotation90];
    pub fn value(self) -> i32 {
        match self {
            Self::Rotation0 => 0,
            Self::Rotation180 => 2,
            Self::Rotation270 => 3,
            Self::Rotation90 => 1,
        }
    }
    pub fn of(value: i32) -> Option<Self> {
        Self::VALUES.into_iter().find(|r| r.value() == value)
    }
}
/// Owned, tightly packed straight-alpha sRGB pixels. No device/adapter ownership.
pub type RgbaFrame = RgbaImage;
pub trait Screenshottable: Send + Sync {
    fn orientation(&self) -> VendorResult<Rotation>;
    /// A source TimeoutException is Other(Failure), never Cancelled(Timeout).
    /// Implementations must join their I/O before returning cancellation.
    fn screenshot<'a>(
        &'a self,
        timeout: Duration,
        cancellation: Arc<dyn CancellationSignal>,
    ) -> VendorFuture<'a, Option<RgbaFrame>>;
}

struct ExplicitStream<W> {
    inner: W,
    enabled: Arc<AtomicBool>,
}
impl<W: Write> Write for ExplicitStream<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if !self.enabled.load(Ordering::Relaxed) {
            return Ok(bytes.len());
        }
        let result = self.inner.write(bytes);
        if result.is_err() {
            self.enabled.store(false, Ordering::Relaxed)
        }
        result
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
pub struct GifSequenceWriter<W: Write> {
    stream: Option<W>,
    encoder: Option<gif::Encoder<ExplicitStream<W>>>,
    delay: u16,
    continuous: bool,
    enabled: Arc<AtomicBool>,
}
impl<W: Write> Drop for GifSequenceWriter<W> {
    fn drop(&mut self) {
        // gif::Encoder otherwise writes a trailer in Drop, even after an error.
        // Only the awaited finish operation may produce observable cleanup.
        self.enabled.store(false, Ordering::Relaxed);
    }
}
impl<W: Write> GifSequenceWriter<W> {
    pub fn new(stream: W, delay_millis: i32, continuous: bool) -> VendorResult<Self> {
        let delay = delay_millis / 10;
        let delay = u16::try_from(delay).map_err(|_| {
            VendorError::Other(failure(
                "javax.imageio.metadata.IIOInvalidTreeException",
                "Bad value for GraphicControlExtension attribute delayTime!",
            ))
        })?;
        Ok(Self {
            stream: Some(stream),
            encoder: None,
            delay,
            continuous,
            enabled: Arc::new(AtomicBool::new(true)),
        })
    }
    pub fn write_to_sequence(&mut self, image: &RgbaFrame) -> VendorResult<()> {
        let (width, height) = (u16::try_from(image.width()), u16::try_from(image.height()));
        let (width, height) = match (width, height) {
            (Ok(w), Ok(h)) if w > 0 && h > 0 => (w, h),
            _ => {
                return Err(VendorError::Other(failure(
                    "java.lang.IllegalArgumentException",
                    "Invalid GIF dimensions",
                )));
            }
        };
        if self.encoder.is_none() {
            let stream = self.stream.take().ok_or_else(|| {
                VendorError::Other(failure("java.lang.IllegalStateException", "GIF already finished"))
            })?;
            self.encoder = Some(
                gif::Encoder::new(
                    ExplicitStream {
                        inner: stream,
                        enabled: self.enabled.clone(),
                    },
                    width,
                    height,
                    &[],
                )
                .map_err(|error| match error {
                    gif::EncodingError::Io(error) => {
                        let mut f = failure("javax.imageio.IIOException", "I/O error writing header!");
                        f.cause = Some(Box::new(io_error(error).failure().clone()));
                        VendorError::Other(f)
                    }
                    other => gif_error(other),
                })?,
            );
        }
        let encoder = self
            .encoder
            .as_mut()
            .ok_or_else(|| VendorError::Other(failure("java.lang.IllegalStateException", "GIF already finished")))?;
        // The JDK emits the application extension on every image. Its malformed
        // CommentExtensions attribute produces no comment (C63).
        encoder
            .set_repeat(if self.continuous {
                gif::Repeat::Infinite
            } else {
                gif::Repeat::Finite(1)
            })
            .map_err(gif_error)?;
        let mut pixels = image.as_raw().clone();
        for pixel in pixels.as_chunks_mut::<4>().0 {
            if pixel[3] < 255 {
                *pixel = [0, 0, 0, 0]
            } else {
                pixel[3] = 255
            }
        }
        let mut frame = gif::Frame::from_rgba_speed(width, height, &mut pixels, 10);
        frame.delay = self.delay;
        frame.dispose = gif::DisposalMethod::Any;
        // ImageIO's palette conversion overrides the supplied FALSE flag when
        // any source alpha is below 255, and reserves transparent index zero.
        if let Some(index) = frame.transparent {
            if index != 0 {
                if let Some(palette) = frame.palette.as_mut() {
                    for channel in 0..3 {
                        palette.swap(channel, index as usize * 3 + channel)
                    }
                }
                for pixel in frame.buffer.to_mut() {
                    if *pixel == index {
                        *pixel = 0
                    } else if *pixel == 0 {
                        *pixel = index
                    }
                }
            }
            frame.transparent = Some(0);
        }
        frame.needs_user_input = false;
        encoder.write_frame(&frame).map_err(gif_error)
    }
    /// Explicitly finishes the sequence and returns the still-open underlying stream.
    pub fn finish(mut self) -> VendorResult<W> {
        self.enabled.store(true, Ordering::Relaxed);
        if let Some(encoder) = self.encoder.take() {
            encoder.into_inner().map(|stream| stream.inner).map_err(io_error)
        } else {
            let mut stream = self.stream.take().ok_or_else(|| {
                VendorError::Other(failure("java.lang.IllegalStateException", "GIF already finished"))
            })?;
            stream.write_all(&[0x3b]).map_err(io_error)?;
            Ok(stream)
        }
    }
}
fn gif_error(error: gif::EncodingError) -> VendorError {
    match error {
        gif::EncodingError::Io(error) => io_error(error),
        other => VendorError::Other(failure("javax.imageio.IIOException", &other.to_string())),
    }
}

/// Persistent first dimensions are intentionally not reset by start (C64).
#[derive(Default)]
pub struct FrameTransform {
    first: Option<(u32, u32)>,
}
impl FrameTransform {
    pub fn transform(
        &mut self,
        image: RgbaFrame,
        orientation: Rotation,
        width: i32,
        height: i32,
    ) -> VendorResult<RgbaFrame> {
        let first = *self.first.get_or_insert(image.dimensions());
        let turns = (orientation.value() + i32::from(image.width() == first.1 && image.height() == first.0)) % 4;
        let image = match turns {
            1 => imageops::rotate90(&image),
            2 => imageops::rotate180(&image),
            3 => imageops::rotate270(&image),
            _ => image,
        };
        if width < 0 || height < 0 {
            return Err(VendorError::Other(failure(
                "java.lang.IllegalArgumentException",
                "targetWidth and targetHeight must be >= 0",
            )));
        }
        let ratio = image.height() as f32 / image.width() as f32;
        let (tw, th) = if image.width() < image.height() {
            (width, height)
        } else {
            (height, width)
        };
        let (tw, th) = if ratio <= 1.0 {
            (tw, jcompat::jnum::kotlin_round_to_int(tw as f32 * ratio).unwrap_or(0))
        } else {
            (jcompat::jnum::kotlin_round_to_int(th as f32 / ratio).unwrap_or(0), th)
        };
        if tw <= 0 || th <= 0 {
            return Err(VendorError::Other(failure(
                "java.lang.IllegalArgumentException",
                "Width and height must be > 0",
            )));
        }
        if image.dimensions() == (tw as u32, th as u32) {
            return Ok(image);
        }
        Ok(RgbaImage::from_fn(tw as u32, th as u32, |x, y| {
            *image.get_pixel(
                jcompat::report_text::awt_nearest_index(x, image.width(), tw as u32),
                jcompat::report_text::awt_nearest_index(y, image.height(), th as u32),
            )
        }))
    }
}
#[derive(Clone)]
pub struct CaptureSettings {
    pub device: DeviceInfo,
    pub pool: DevicePoolId,
    pub batch_id: String,
    pub delay: Duration,
    pub height: i32,
    pub width: i32,
    pub timeout: Duration,
}
#[derive(Clone)]
pub struct CaptureServices {
    pub io: Arc<dyn VendorRuntime>,
    pub clock: Arc<dyn VendorClock>,
    pub wall: Arc<dyn WallClock>,
    pub files: Arc<dyn OutputFiles>,
    pub log: ServiceLog,
}
pub struct ScreenCapturer {
    settings: CaptureSettings,
    services: CaptureServices,
    source: Arc<dyn Screenshottable>,
    transform: Arc<Mutex<FrameTransform>>,
    captures: Arc<AtomicI32>,
    logger: Logger,
}
impl ScreenCapturer {
    pub fn new(
        settings: CaptureSettings,
        services: CaptureServices,
        source: Arc<dyn Screenshottable>,
        registry: &mut MarathonLogging,
    ) -> Self {
        Self {
            settings,
            services,
            source,
            transform: Arc::new(Mutex::new(FrameTransform::default())),
            captures: Arc::new(AtomicI32::new(0)),
            logger: registry.logger("com.malinskiy.marathon.device.screenshot.ScreenCapturer", None),
        }
    }
    pub fn captures_taken(&self) -> i32 {
        self.captures.load(Ordering::Acquire)
    }
    /// 6.6b owns launch/cancel-and-join. Returning means the encoder and stream
    /// cleanup have finished, including cancellation during capture/write/delay.
    pub fn start<'a>(&'a mut self, test: &Test, cancellation: Arc<dyn CancellationSignal>) -> VendorFuture<'a, ()> {
        self.capture(test, cancellation)
    }
    /// The `'static` form for a spawned capture job. 6.6b keeps one `Arc<ScreenCapturer>`
    /// per device, spawns `start_owned` beside the test batch, then cancels and joins
    /// the task before reading `captures_taken` or starting the next test, as Kotlin's
    /// launch/cancelAndJoin does. Concurrent starts would share the counter and the
    /// first-frame dimensions, so the caller must not overlap them.
    pub fn start_owned(
        self: Arc<Self>,
        test: &Test,
        cancellation: Arc<dyn CancellationSignal>,
    ) -> VendorFuture<'static, ()> {
        self.capture(test, cancellation)
    }
    fn capture(&self, test: &Test, cancellation: Arc<dyn CancellationSignal>) -> VendorFuture<'static, ()> {
        let (settings, services, source, transform, captures, logger, test) = (
            self.settings.clone(),
            self.services.clone(),
            self.source.clone(),
            self.transform.clone(),
            self.captures.clone(),
            self.logger.clone(),
            test.clone(),
        );
        let io = self.services.io.clone();
        Box::pin(async move {
            on_io(
                io.as_ref(),
                Box::pin(async move {
                    captures.store(0, Ordering::Release);
                    let target = FileTarget {
                        kind: FileType::Screenshot,
                        pool: settings.pool,
                        device: settings.device,
                        test: Some(test),
                        test_batch_id: Some(settings.batch_id),
                        id: None,
                        chunk: None,
                    };
                    let path = services.files.create_file(&target).await.map_err(vendor_error)?;
                    // Kotlin opens the stream before its lazy delayMillis overflows,
                    // so an invalid delay still leaves the empty file behind.
                    let delay = settings.delay.to_millis();
                    let writer = blocking(services.io.as_ref(), move || {
                        validate_path(&path)?;
                        let stream = std::fs::OpenOptions::new()
                            .create(true)
                            .truncate(false)
                            .read(true)
                            .write(true)
                            .open(&path)
                            .map_err(|e| file_open_error(e, &path))?;
                        let delay = delay.ok_or_else(|| {
                            VendorError::Other(failure("java.lang.ArithmeticException", "long overflow"))
                        })? as i32;
                        Ok((GifSequenceWriter::new(stream, delay, true)?, delay))
                    })
                    .await?;
                    let (writer, delay) = writer;
                    let mut writer = Some(writer);
                    let outcome = async {
                        let orientation = source.orientation()?;
                        loop {
                            if let Some(reason) = cancellation.reason() {
                                return Err(VendorError::Cancelled(reason));
                            }
                            let start = services
                                .wall
                                .millis("core.ScreenCapturer.capture")
                                .map_err(clock_error)?;
                            let frame = match source.screenshot(settings.timeout, cancellation.clone()).await {
                                Err(VendorError::Other(ref f))
                                    if f.class == "java.util.concurrent.TimeoutException" =>
                                {
                                    (services.log)(&logger, Level::Error, "Timeout. Exiting", Some(f));
                                    None
                                }
                                Err(VendorError::Other(ref f)) if f.class == "java.io.IOException" => None,
                                other => other?,
                            };
                            if let Some(frame) = frame {
                                let transform = transform.clone();
                                let mut owned = writer.take().ok_or_else(|| {
                                    VendorError::Other(failure("java.lang.IllegalStateException", "Missing GIF writer"))
                                })?;
                                // Return the writer even on a frame failure so finally can finish it.
                                let (next, result) = blocking(services.io.as_ref(), move || {
                                    let result = transform
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .transform(frame, orientation, settings.width, settings.height)
                                        .and_then(|image| owned.write_to_sequence(&image));
                                    Ok((owned, result))
                                })
                                .await?;
                                writer = Some(next);
                                result?;
                                captures.fetch_add(1, Ordering::Release);
                            }
                            let elapsed = services
                                .wall
                                .millis("core.ScreenCapturer.capture")
                                .map_err(clock_error)?
                                .wrapping_sub(start);
                            let sleep = (delay as i64).wrapping_sub(elapsed).max(0);
                            // VendorClock is the frozen scheduler seam; the call site is
                            // core.ScreenCapturer.scheduler.L55 in the replay inventory.
                            services.clock.delay(sleep, cancellation.clone()).await?;
                        }
                    }
                    .await;
                    // Kotlin's finally exception replaces the body error. FileImageOutputStream
                    // closes after endWriteSequence, not before it.
                    blocking(services.io.as_ref(), move || {
                        let Some(writer) = writer else { return Ok(()) };
                        let mut stream = writer.finish()?;
                        stream.flush().map_err(io_error)?;
                        nix::unistd::close(stream).map_err(|e| io_error(std::io::Error::from_raw_os_error(e as i32)))
                    })
                    .await?;
                    outcome
                }),
            )
            .await
        })
    }
}
