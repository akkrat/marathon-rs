//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/request/framebuffer/*.kt`.
//! Copyright (C) 2019-2021 Anton Malinskiy; Apache-2.0.
//!
//! Pixel conversion and PNG encoding are synchronous CPU work. Call `decode`/`encode_png` through
//! the owning vendor task's `spawn_blocking` and join it (6.5/6.6b); capture itself only reads bytes.
//! P4 replaces recycled AWT identity with owned images, retaining profile metadata and pixel values.

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
    pub fn profile(self) -> Option<&'static [u8]> {
        match self {
            Self::Unknown => None,
            Self::Srgb => Some(include_bytes!("../../../../fixtures/adam/colorProfiles/sRGB.icc")),
            Self::P3 => Some(include_bytes!("../../../../fixtures/adam/colorProfiles/DisplayP3.icc")),
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
    /// sRGB RGBA, equivalent to BufferedImage.getRGB(), after the selected ICC conversion.
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
fn expanded(value: u32, offset: u32, length: u32) -> Result<u8, AdbError> {
    let v = (value.wrapping_shr(offset)) & mask(length)?;
    Ok(if length < 8 {
        (v << (8 - length)) + ((1 << (8 - length)) - 1)
    } else {
        v >> (length - 8)
    } as u8)
}
impl RawImage {
    pub fn get_argb(&self, index: usize) -> Result<u32, AdbError> {
        let b = self.buffer.get(index..).ok_or_else(malformed)?;
        match self.bits_per_pixel {
            16 if b.len() >= 2 => {
                let v = u32::from(u16::from_le_bytes([b[0], b[1]]));
                Ok(0xff000000
                    | (((v >> 11) & 31) * 255 / 31) << 16
                    | (((v >> 5) & 63) * 255 / 63) << 8
                    | ((v & 31) * 255 / 31))
            }
            32 if b.len() >= 4 => {
                let v = word(b, 0);
                let part = |o, l| -> Result<u32, AdbError> {
                    Ok((v.wrapping_shr(o) & mask(l)?).wrapping_shl(8u32.wrapping_sub(l)))
                };
                Ok(part(self.alpha_offset, self.alpha_length)?.wrapping_shl(24)
                    | part(self.red_offset, self.red_length)?.wrapping_shl(16)
                    | part(self.green_offset, self.green_length)?.wrapping_shl(8)
                    | part(self.blue_offset, self.blue_length)?)
            }
            _ => Err(AdbError::UnsupportedOperation(
                "RawImage.getARGB(int) only works in 16 and 32 bit mode.".into(),
            )),
        }
    }
    pub fn to_buffered_image(&self) -> Result<FramebufferImage, AdbError> {
        let mut rgba = Vec::with_capacity(self.width as usize * self.height as usize * 4);
        for i in (0..self.buffer.len()).step_by((self.bits_per_pixel / 8).max(1) as usize) {
            let v = self.get_argb(i)?;
            rgba.extend_from_slice(&[(v >> 16) as u8, (v >> 8) as u8, v as u8, (v >> 24) as u8]);
        }
        // setRGB quantizes into the profile's 8-bit raster before getRGB converts back to sRGB.
        if let Some(profile) = self.color_space.and_then(ColorSpace::profile) {
            convert_profile(&mut rgba, JDK_SRGB, profile)?;
            convert_profile(&mut rgba, profile, JDK_SRGB)?;
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
        if raw.bits_per_pixel == 16 && raw.color_space.and_then(ColorSpace::profile).is_some() {
            return Err(AdbError::Runtime("Unsupported buffered image type 8".into()));
        }
        let pixels = u64::from(raw.width) * u64::from(raw.height);
        if pixels * u64::from(raw.bits_per_pixel / 8) != raw.buffer.len() as u64 {
            return Err(malformed());
        }
        let aligned = raw.alpha_offset == 24
            && raw.alpha_length == 8
            && raw.blue_offset == 16
            && raw.blue_length == 8
            && raw.green_offset == 8
            && raw.green_length == 8
            && raw.red_offset == 0
            && raw.red_length == 8;
        let mut rgba = Vec::with_capacity(pixels as usize * 4);
        for b in raw.buffer.chunks_exact((raw.bits_per_pixel / 8) as usize) {
            if raw.bits_per_pixel == 16 {
                // ByteBuffer.asShortBuffer is big-endian; AWT rounds the component scaling.
                let v = u32::from(u16::from_be_bytes([b[0], b[1]]));
                rgba.extend_from_slice(&[
                    ((((v >> 11) & 31) * 255 + 15) / 31) as u8,
                    ((((v >> 5) & 63) * 255 + 31) / 63) as u8,
                    (((v & 31) * 255 + 15) / 31) as u8,
                    255,
                ]);
            } else if aligned {
                rgba.extend_from_slice(b);
            } else {
                let value = word(b, 0);
                let r = expanded(value, raw.red_offset, raw.red_length)?;
                let g = expanded(value, raw.green_offset, raw.green_length)?;
                let b = expanded(value, raw.blue_offset, raw.blue_length)?;
                let a = expanded(value, raw.alpha_offset, raw.alpha_length)?;
                let alpha = f64::from(a) / 255.0;
                rgba.extend_from_slice(&[
                    (f64::from(r) * alpha).round() as u8,
                    (f64::from(g) * alpha).round() as u8,
                    (f64::from(b) * alpha).round() as u8,
                    255,
                ]);
            }
        }
        if let Some(profile) = raw.color_space.and_then(ColorSpace::profile) {
            convert_profile(&mut rgba, profile, JDK_SRGB)?;
        }
        Ok(FramebufferImage {
            width: raw.width,
            height: raw.height,
            rgba,
            source_color_space: raw.color_space,
        })
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

const JDK_SRGB: &[u8] = include_bytes!("../../../../fixtures/oracle/adb-request-jdk-srgb.icc");
fn convert_profile(rgba: &mut [u8], input_profile: &[u8], output_profile: &[u8]) -> Result<(), AdbError> {
    let input = Profile::new_icc(input_profile).map_err(|e| AdbError::Io(e.to_string()))?;
    let output = Profile::new_icc(output_profile).map_err(|e| AdbError::Io(e.to_string()))?;
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
