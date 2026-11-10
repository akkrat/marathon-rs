//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/request/framebuffer/*.kt`.
//! Copyright (C) 2019-2021 Anton Malinskiy; Apache-2.0.
//!
//! Pixel conversion and PNG encoding are synchronous CPU work. Call `decode`/`encode_png` through
//! the owning vendor task's `spawn_blocking` and join it (6.5/6.6b); capture itself only reads bytes.
//! D63 uses one straight-alpha decoder and standard sRGB output, retaining source color metadata.

use super::{AdbError, ComplexRequest, Request, Socket, Target, request::create_base_request};
use lcms2::{Intent, PixelFormat, Profile, Transform};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorSpace {
    Unknown,
    Srgb,
    P3,
}
impl ColorSpace {
    pub fn from_value(value: u32) -> Self {
        match value {
            1 => Self::Srgb,
            2 => Self::P3,
            _ => Self::Unknown,
        }
    }
    pub fn profile_name(self) -> Option<&'static str> {
        match self {
            Self::Unknown => None,
            Self::Srgb => Some("sRGB.icc"),
            Self::P3 => Some("DisplayP3.icc"),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawImage {
    pub version: u32,
    pub bits_per_pixel: u32,
    pub color_space: Option<ColorSpace>,
    pub size: u32,
    pub width: u32,
    pub height: u32,
    pub red_offset: u32,
    pub red_length: u32,
    pub blue_offset: u32,
    pub blue_length: u32,
    pub green_offset: u32,
    pub green_length: u32,
    pub alpha_offset: u32,
    pub alpha_length: u32,
    pub buffer: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FramebufferImage {
    pub width: u32,
    pub height: u32,
    /// Straight-alpha sRGB RGBA after any source color conversion.
    pub rgba: Vec<u8>,
    /// The source framebuffer's profile; rgba itself is already converted to sRGB.
    pub source_color_space: Option<ColorSpace>,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct RawImageScreenCaptureAdapter;
#[derive(Clone, Copy, Debug, Default)]
pub struct BufferedImageScreenCaptureAdapter;

fn malformed() -> AdbError {
    AdbError::UnsupportedImageProtocol("Malformed framebuffer record".into())
}
async fn read(socket: &mut Socket, n: usize) -> Result<Vec<u8>, AdbError> {
    let mut b = vec![0; n];
    if socket.read_fully(&mut b).await != n {
        return Err(malformed());
    }
    Ok(b)
}
fn word(b: &[u8], n: usize) -> u32 {
    let i = n * 4;
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

pub struct ScreenCaptureRequest;
impl Request for ScreenCaptureRequest {
    fn serialize(&self) -> Vec<u8> {
        create_base_request(&Target::NonSpecified, "framebuffer:")
    }
}
impl ComplexRequest for ScreenCaptureRequest {
    type Output = RawImage;
    async fn read_element(&mut self, s: &mut Socket) -> Result<RawImage, AdbError> {
        let version = word(&read(s, 4).await?, 0);
        let count = match version {
            1 => 12,
            2 => 13,
            16 => 3,
            _ => {
                return Err(AdbError::UnsupportedImageProtocol(format!(
                    // Kotlin reads the version as a signed Int.
                    "Unsupported version {}",
                    version as i32
                )));
            }
        };
        let header = read(s, count * 4).await?;
        s.write(&[0]).await?;
        let (bpp, color_space, start) = if version == 16 {
            (16, None, 0)
        } else if version == 2 {
            (word(&header, 0), Some(ColorSpace::from_value(word(&header, 1))), 2)
        } else {
            (word(&header, 0), None, 1)
        };
        let size = word(&header, start);
        let width = word(&header, start + 1);
        let height = word(&header, start + 2);
        let geometry = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|n| n.checked_mul(u64::from(bpp / 8)))
            .ok_or_else(malformed)?;
        if size > 256 * 1024 * 1024
            || width == 0
            || height == 0
            || geometry != u64::from(size)
            || !bpp.is_multiple_of(8)
        {
            return Err(malformed());
        }
        let channels = if version == 16 {
            [11, 5, 0, 5, 5, 6, 0, 0]
        } else {
            std::array::from_fn(|i| word(&header, start + 3 + i))
        };
        Ok(RawImage {
            version,
            bits_per_pixel: bpp,
            color_space,
            size,
            width,
            height,
            red_offset: channels[0],
            red_length: channels[1],
            blue_offset: channels[2],
            blue_length: channels[3],
            green_offset: channels[4],
            green_length: channels[5],
            alpha_offset: channels[6],
            alpha_length: channels[7],
            buffer: read(s, size as usize).await?,
        })
    }
}
fn mask(length: u32) -> Result<u32, AdbError> {
    if (1..=10).contains(&length) {
        Ok((1 << length) - 1)
    } else {
        Err(AdbError::Runtime(format!("Unexpected mask length {length}")))
    }
}
// The old Java 8-length shift erased RGB10 samples (U190); normalize their full range.
fn component(value: u32, offset: u32, length: u32, pixel_width: u32) -> Result<u32, AdbError> {
    let maximum = mask(length)?;
    if offset >= pixel_width || length > pixel_width - offset {
        return Err(malformed());
    }
    // Keep the established 16-bit floor scaling; 32-bit channels round to nearest (D63).
    let rounding = if pixel_width == 16 { 0 } else { maximum / 2 };
    Ok((((value >> offset) & maximum) * 255 + rounding) / maximum)
}
impl RawImage {
    pub fn get_argb(&self, index: usize) -> Result<u32, AdbError> {
        let b = self.buffer.get(index..).ok_or_else(malformed)?;
        let value = match self.bits_per_pixel {
            16 if b.len() >= 2 => u32::from(u16::from_le_bytes([b[0], b[1]])),
            32 if b.len() >= 4 => word(b, 0),
            _ => {
                return Err(AdbError::UnsupportedOperation(
                    "RawImage.getARGB(int) only works in 16 and 32 bit mode.".into(),
                ));
            }
        };
        // The layout-less legacy image is RGB565. Wire compatibility value 16
        // already gets these defaults when its three-word header is read.
        let (red, green, blue, alpha) = if self.version == 0 && self.bits_per_pixel == 16 {
            ((11, 5), (5, 6), (0, 5), (0, 0))
        } else {
            (
                (self.red_offset, self.red_length),
                (self.green_offset, self.green_length),
                (self.blue_offset, self.blue_length),
                (self.alpha_offset, self.alpha_length),
            )
        };
        let channel = |(offset, length)| component(value, offset, length, self.bits_per_pixel);
        let alpha = if alpha.1 == 0 { 255 } else { channel(alpha)? };
        Ok(alpha << 24 | channel(red)? << 16 | channel(green)? << 8 | channel(blue)?)
    }
    pub fn to_buffered_image(&self) -> Result<FramebufferImage, AdbError> {
        if !matches!(self.bits_per_pixel, 16 | 32) {
            return Err(AdbError::UnsupportedOperation(
                "RawImage.getARGB(int) only works in 16 and 32 bit mode.".into(),
            ));
        }
        let bytes = u64::from(self.width)
            .checked_mul(u64::from(self.height))
            .and_then(|n| n.checked_mul(u64::from(self.bits_per_pixel / 8)))
            .ok_or_else(malformed)?;
        if self.width == 0
            || self.height == 0
            || bytes > 256 * 1024 * 1024
            || bytes != u64::from(self.size)
            || bytes != self.buffer.len() as u64
        {
            return Err(malformed());
        }
        let mut rgba = Vec::with_capacity(self.width as usize * self.height as usize * 4);
        for i in (0..self.buffer.len()).step_by((self.bits_per_pixel / 8) as usize) {
            let v = self.get_argb(i)?;
            rgba.extend_from_slice(&[(v >> 16) as u8, (v >> 8) as u8, v as u8, (v >> 24) as u8]);
        }
        if self.color_space == Some(ColorSpace::P3) {
            convert_p3_to_srgb(&mut rgba)?;
        }
        Ok(FramebufferImage {
            width: self.width,
            height: self.height,
            rgba,
            source_color_space: self.color_space,
        })
    }
}
impl BufferedImageScreenCaptureAdapter {
    pub fn decode(&self, raw: &RawImage) -> Result<FramebufferImage, AdbError> {
        if !matches!(raw.bits_per_pixel, 16 | 32) {
            return Err(AdbError::UnsupportedOperation(
                "BufferedImageScreenCaptureAdapter only works with 16 and 32 bit images".into(),
            ));
        }
        raw.to_buffered_image()
    }
}
impl FramebufferImage {
    pub fn encode_png(&self) -> Result<Vec<u8>, AdbError> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, self.width, self.height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
            let mut writer = encoder.write_header().map_err(|e| AdbError::Io(e.to_string()))?;
            writer
                .write_image_data(&self.rgba)
                .map_err(|e| AdbError::Io(e.to_string()))?;
        }
        Ok(out)
    }
}
#[cfg(test)]
mod tests;

fn convert_p3_to_srgb(rgba: &mut [u8]) -> Result<(), AdbError> {
    let input = Profile::new_icc(include_bytes!("../../../../fixtures/adam/colorProfiles/DisplayP3.icc"))
        .map_err(|e| AdbError::Io(e.to_string()))?;
    let output = Profile::new_srgb();
    let transform = Transform::<[u16; 3], [u16; 3]>::new_flags(
        &input,
        PixelFormat::RGB_16,
        &output,
        PixelFormat::RGB_16,
        Intent::Perceptual,
        lcms2::Flags::NO_OPTIMIZE,
    )
    .map_err(|e| AdbError::Io(e.to_string()))?;
    let mut rgb: Vec<[u16; 3]> = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| [u16::from(b[0]) * 257, u16::from(b[1]) * 257, u16::from(b[2]) * 257])
        .collect();
    transform.transform_in_place(&mut rgb);
    for (dst, src) in rgba.as_chunks_mut::<4>().0.iter_mut().zip(rgb) {
        for i in 0..3 {
            dst[i] = ((u32::from(src[i]) + 128) / 257) as u8;
        }
    }

    Ok(())
}
