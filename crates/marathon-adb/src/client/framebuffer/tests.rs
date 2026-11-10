//! Port of adam 0.5.8 `adam/src/test/kotlin/com/malinskiy/adam/request/framebuffer/ScreenCaptureRequestTest.kt`
//! and `.../framebuffer/screencapture/BufferedImageScreenCaptureAdapterTest.kt`.
//! Copyright (C) 2021 Anton Malinskiy; Apache-2.0.
use super::*;
use crate::client::stub;
use sha2::{Digest, Sha256};
fn oracle(case: &str) -> String {
    include_str!("../../../../../fixtures/oracle/adb-request-probes.jsonl")
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|v| v["case"] == case)
        .unwrap()["value"]
        .as_str()
        .unwrap()
        .into()
}
fn fixture(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/adam/fixture")
        .join(name)
}
async fn capture(name: &str) -> RawImage {
    let b = tokio::fs::read(fixture(&format!("{name}.bin"))).await.unwrap();
    let version = if name == "screencap_3" { 2u32 } else { 1 };
    let size = if version == 2 { 52 } else { 48 };
    let s = stub::session(move |mut s| async move {
        s.expect_cmd("host:transport:serial").await;
        s.accept().await;
        s.expect_cmd("framebuffer:").await;
        s.accept().await;
        s.respond(&version.to_le_bytes()).await;
        s.respond(&b[..size]).await;
        s.expect_bytes(&[0]).await;
        s.respond(&b[size..]).await;
    })
    .await;
    let raw = s.client().execute(ScreenCaptureRequest, Some("serial")).await.unwrap();
    s.finish().await;
    raw
}
fn pixels(image: &FramebufferImage) -> Vec<u8> {
    image
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[3], p[0], p[1], p[2]])
        .collect()
}
fn png_rgba(bytes: Vec<u8>) -> Vec<u8> {
    let mut d = png::Decoder::new(std::io::Cursor::new(bytes));
    d.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = d.read_info().unwrap();
    let mut out = vec![0; r.output_buffer_size().unwrap()];
    let info = r.next_frame(&mut out).unwrap();
    let out = &out[..info.buffer_size()];
    match info.color_type {
        png::ColorType::Rgba => out.to_vec(),
        png::ColorType::Rgb => out
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        other => panic!("{other:?}"),
    }
}
async fn buffered(name: &'static str, png_name: &'static str) {
    let adapter = BufferedImageScreenCaptureAdapter;
    for _ in 0..2 {
        let raw = capture(name).await;
        let source = raw.buffer.clone();
        let expected_png = tokio::fs::read(fixture(png_name)).await.unwrap();
        let image = tokio::task::spawn_blocking(move || adapter.decode(&raw))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(image.width, 1080);
        assert_eq!(image.height, 1920);
        let hash = format!("{:x}", Sha256::digest(pixels(&image)));
        if matches!(name, "screencap_1" | "screencap_2") {
            assert_eq!(hash, oracle(&format!("frame-argb-sha256:{name}")), "{name}");
        }
        if name == "screencap_3" {
            assert_eq!(image.rgba, source, "sRGB samples pass through unchanged under D63");
        }
        let expected = png_rgba(expected_png);
        assert!(
            image
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .zip(expected.as_chunks::<4>().0.iter())
                .all(|(a, b)| a.iter().zip(b).all(|(x, y)| x.abs_diff(*y) <= 1)),
            "fixture PNG differs by more than one component level"
        );
        let copy = image.clone();
        let png = tokio::task::spawn_blocking(move || copy.encode_png())
            .await
            .unwrap()
            .unwrap();
        assert!(png_rgba(png) == image.rgba, "PNG round trip");
    }
}
mod screen_capture_request_test {
    use super::*;
    #[tokio::test]
    async fn test_protocol1() {
        let raw = capture("screencap_1").await;
        assert_eq!(
            (raw.version, raw.bits_per_pixel, raw.size, raw.width, raw.height),
            (1, 32, 8294400, 1080, 1920)
        );
        assert_eq!(
            [
                raw.red_offset,
                raw.red_length,
                raw.green_offset,
                raw.green_length,
                raw.blue_offset,
                raw.blue_length,
                raw.alpha_offset,
                raw.alpha_length
            ],
            [0, 8, 8, 8, 16, 8, 24, 8]
        );
        let hash = raw
            .buffer
            .iter()
            .fold(1i32, |h, b| h.wrapping_mul(31).wrapping_add(i32::from(*b as i8)));
        assert_eq!(hash, -1474724227);
        let png = tokio::fs::read(fixture("screencap_1.png")).await.unwrap();
        assert_eq!(raw.to_buffered_image().unwrap().rgba, png_rgba(png));
    }
    #[tokio::test]
    async fn test_protocol16bit() {
        let raw = capture("screencap_2").await;
        assert_eq!(
            (raw.version, raw.bits_per_pixel, raw.size, raw.width, raw.height),
            (1, 16, 4147200, 1080, 1920)
        );
        assert_eq!(
            [
                raw.red_offset,
                raw.red_length,
                raw.green_offset,
                raw.green_length,
                raw.blue_offset,
                raw.blue_length,
                raw.alpha_offset,
                raw.alpha_length
            ],
            [11, 5, 5, 6, 0, 5, 0, 0]
        );
        assert_eq!(
            raw.buffer
                .iter()
                .fold(1i32, |h, b| h.wrapping_mul(31).wrapping_add(i32::from(*b as i8))),
            1300692993
        );
        let png = tokio::fs::read(fixture("screencap_2.png")).await.unwrap();
        assert_eq!(raw.to_buffered_image().unwrap().rgba, png_rgba(png));
    }
    #[tokio::test]
    async fn test_with_buffered_image_adapter() {
        buffered("screencap_1", "screencap_1.png").await;
    }
    #[tokio::test]
    async fn test_with_buffered_image_adapter_16_bit() {
        buffered("screencap_2", "screencap_2.png").await;
    }
    #[tokio::test]
    async fn test_with_buffered_image_adapter_unaligned_32_bit() {
        buffered("screencap_1_unaligned", "screencap_1.png").await;
    }
    #[tokio::test]
    async fn test_with_buffered_image_adapter_with_srgb_color_model() {
        buffered("screencap_3", "screencap_3.png").await;
    }
    #[tokio::test]
    async fn test_protocol_unsupported() {
        let s = stub::session(|mut s| async move {
            s.expect_cmd("framebuffer:").await;
            s.accept().await;
            s.respond(&99u32.to_le_bytes()).await;
        })
        .await;
        assert_eq!(
            s.client().execute(ScreenCaptureRequest, None).await.unwrap_err(),
            AdbError::UnsupportedImageProtocol("Unsupported version 99".into())
        );
        s.finish().await;
    }
}
// adam reads the version as a signed Int, so 0xFFFFFFFF reports -1.
#[tokio::test]
async fn unsupported_version_is_formatted_signed() {
    let s = stub::session(|mut s| async move {
        s.expect_cmd("framebuffer:").await;
        s.accept().await;
        s.respond(&u32::MAX.to_le_bytes()).await;
    })
    .await;
    assert_eq!(
        s.client().execute(ScreenCaptureRequest, None).await.unwrap_err(),
        AdbError::UnsupportedImageProtocol("Unsupported version -1".into())
    );
    s.finish().await;
}
mod buffered_image_screen_capture_adapter_test {
    use super::*;
    #[test]
    fn test_throws_exception_if_unsupported_image() {
        let raw = RawImage {
            version: 1,
            bits_per_pixel: 24,
            size: 1,
            width: 1,
            height: 1,
            color_space: None,
            red_offset: 0,
            red_length: 0,
            blue_offset: 0,
            blue_length: 0,
            green_offset: 0,
            green_length: 0,
            alpha_offset: 0,
            alpha_length: 0,
            buffer: vec![0],
        };
        assert_eq!(
            BufferedImageScreenCaptureAdapter.decode(&raw).unwrap_err(),
            AdbError::UnsupportedOperation(
                "BufferedImageScreenCaptureAdapter only works with 16 and 32 bit images".into()
            )
        );
    }
}
#[tokio::test]
async fn profile_transitions_and_protocol16_use_native_pixels_d63() {
    for version in [2u32, 16] {
        for color in [0u32, 1, 2] {
            let mut header = vec![version];
            header.extend(if version == 16 {
                vec![2, 1, 1]
            } else {
                vec![32, color, 4, 1, 1, 0, 8, 16, 8, 8, 8, 24, 8]
            });
            let bytes: Vec<u8> = header.into_iter().flat_map(u32::to_le_bytes).collect();
            let s = stub::session(move |mut s| async move {
                s.expect_cmd("framebuffer:").await;
                s.accept().await;
                s.respond(&bytes).await;
                s.expect_bytes(&[0]).await;
                s.respond(if version == 16 { &[0, 248] } else { &[90, 130, 170, 128] })
                    .await;
            })
            .await;
            let raw = s.client().execute(ScreenCaptureRequest, None).await.unwrap();
            s.finish().await;
            let image = BufferedImageScreenCaptureAdapter.decode(&raw).unwrap();
            let p = &image.rgba;
            let argb = i32::from_be_bytes([p[3], p[0], p[1], p[2]]);
            let expected = if version == 16 {
                [255, 0, 0, 255]
            } else if color == 2 {
                [77, 131, 174, 128]
            } else {
                [90, 130, 170, 128]
            };
            assert_eq!(
                argb,
                i32::from_be_bytes([expected[3], expected[0], expected[1], expected[2]]),
                "version {version} color {color}"
            );
        }
    }
}

#[tokio::test]
async fn raw_profile_values_match_the_shared_decoder_d63() {
    for color in [0u32, 1, 2] {
        let words = [2u32, 32, color, 4, 1, 1, 0, 8, 16, 8, 8, 8, 24, 8];
        let bytes: Vec<u8> = words.into_iter().flat_map(u32::to_le_bytes).collect();
        let s = stub::session(move |mut s| async move {
            s.expect_cmd("framebuffer:").await;
            s.accept().await;
            s.respond(&bytes).await;
            s.expect_bytes(&[0]).await;
            s.respond(&[90, 130, 170, 128]).await;
        })
        .await;
        let raw = s.client().execute(ScreenCaptureRequest, None).await.unwrap();
        s.finish().await;
        let image = raw.to_buffered_image().unwrap();
        let p = &image.rgba;
        let argb = i32::from_be_bytes([p[3], p[0], p[1], p[2]]);
        let expected = if color == 2 {
            [77, 131, 174, 128]
        } else {
            [90, 130, 170, 128]
        };
        assert_eq!(
            argb,
            i32::from_be_bytes([expected[3], expected[0], expected[1], expected[2]])
        );
        assert_eq!(image, BufferedImageScreenCaptureAdapter.decode(&raw).unwrap());
    }
}
#[tokio::test]
async fn malformed_geometry_and_short_pixels_fail() {
    for case in 0..3 {
        let s = stub::session(move |mut s| async move {
            s.expect_cmd("framebuffer:").await;
            s.accept().await;
            if case == 0 {
                s.respond(&[1, 0, 0]).await;
                return;
            }
            let size = if case == 1 { 100 } else { 4 };
            let bytes: Vec<u8> = [1u32, 32, size, 1, 1, 0, 8, 16, 8, 8, 8, 24, 8]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect();
            s.respond(&bytes).await;
            s.expect_bytes(&[0]).await;
            if case == 2 {
                s.respond(&[1]).await;
            }
        })
        .await;
        assert!(matches!(
            s.client().execute(ScreenCaptureRequest, None).await,
            Err(AdbError::UnsupportedImageProtocol(_))
        ));
        s.finish().await;
    }
}

#[tokio::test]
async fn opaque_eight_bit_png_bytes_are_pinned_d63() -> Result<(), Box<dyn std::error::Error>> {
    let raw = capture("screencap_1").await;
    let image = raw.to_buffered_image()?;
    assert_eq!(image, BufferedImageScreenCaptureAdapter.decode(&raw)?);
    let png = image.encode_png()?;
    assert_eq!(png.len(), 395_051);
    assert_eq!(
        format!("{:x}", Sha256::digest(&png)),
        "4c685a153a993f373fc74cd4b614e75d175bd76f0d4e3f00b3d5c93159939025"
    );
    assert!(png.windows(5).any(|bytes| bytes == b"sRGB\x00"));
    Ok(())
}

#[tokio::test]
async fn ten_bit_framebuffer_matches_independent_known_pixels_u190() -> Result<(), Box<dyn std::error::Error>> {
    let raw = capture("screencap_1_unaligned").await;
    let image = raw.to_buffered_image()?;
    // Header: R/G/B occupy bits 0..10/10..20/20..30, alpha bits 30..32.
    // For example 0xde868234 contains 564,416,488,3: nearest 255/1023
    // normalization gives 141,104,122 and full alpha. See the independent proof.
    for (x, y, word, expected) in [
        (0, 0, 0xde868234u32, [141, 104, 122, 255]),
        (500, 600, 0xef49a3b8, [237, 154, 188, 255]),
        (1079, 1919, 0xd4c3511c, [71, 53, 83, 255]),
        (100, 100, 0xebc9532c, [202, 149, 174, 255]),
    ] {
        let index = (y * 1080 + x) * 4;
        assert_eq!(&raw.buffer[index..index + 4], &word.to_le_bytes());
        assert_eq!(&image.rgba[index..index + 4], &expected);
        assert_eq!(
            raw.get_argb(index)?,
            u32::from_be_bytes([expected[3], expected[0], expected[1], expected[2]])
        );
    }
    assert_eq!(image, BufferedImageScreenCaptureAdapter.decode(&raw)?);
    assert_eq!(image.encode_png()?.len(), 394_679);
    Ok(())
}

fn packed_pixel(bytes: Vec<u8>) -> RawImage {
    RawImage {
        version: 2,
        bits_per_pixel: 32,
        color_space: None,
        size: bytes.len() as u32,
        width: (bytes.len() / 4) as u32,
        height: 1,
        red_offset: 0,
        red_length: 8,
        green_offset: 8,
        green_length: 8,
        blue_offset: 16,
        blue_length: 8,
        alpha_offset: 24,
        alpha_length: 8,
        buffer: bytes,
    }
}

#[test]
fn rgba8888_copy_honors_all_channel_layout_fields_d63() -> Result<(), Box<dyn std::error::Error>> {
    let canonical = packed_pixel(vec![0x12, 0x34, 0x56, 0x78]);
    for case in 0..10 {
        let mut raw = canonical.clone();
        let mut expected = [0x12, 0x34, 0x56, 0x78];
        match case {
            0 => {}
            1 => {
                raw.red_offset = 8;
                expected[0] = 0x34;
            }
            2 => {
                raw.green_offset = 16;
                expected[1] = 0x56;
            }
            3 => {
                raw.blue_offset = 24;
                expected[2] = 0x78;
            }
            4 => {
                raw.alpha_offset = 0;
                expected[3] = 0x12;
            }
            5 => {
                raw.red_length = 4;
                expected[0] = 34;
            }
            6 => {
                raw.green_length = 4;
                expected[1] = 68;
            }
            7 => {
                raw.blue_length = 4;
                expected[2] = 102;
            }
            8 => {
                raw.alpha_length = 4;
                expected[3] = 136;
            }
            _ => {
                raw.alpha_length = 0;
                expected[3] = 255;
            }
        }
        assert_eq!(raw.to_buffered_image()?.rgba, expected, "case {case}");
        assert_eq!(
            BufferedImageScreenCaptureAdapter.decode(&raw)?.rgba,
            expected,
            "case {case}"
        );
    }
    Ok(())
}

#[test]
fn straight_alpha_channel_widths_and_missing_alpha_d63() -> Result<(), Box<dyn std::error::Error>> {
    let mut raw = packed_pixel(vec![90, 130, 170, 0, 90, 130, 170, 128, 90, 130, 170, 255]);
    assert_eq!(raw.to_buffered_image()?.rgba, raw.buffer);
    assert_eq!(
        BufferedImageScreenCaptureAdapter.decode(&raw)?,
        raw.to_buffered_image()?
    );
    raw.alpha_length = 0;
    assert_eq!(raw.to_buffered_image()?.rgba, [90, 130, 170, 255].repeat(3));
    raw = packed_pixel(0x80000000u32.to_le_bytes().to_vec());
    raw.red_length = 10;
    raw.green_offset = 10;
    raw.green_length = 10;
    raw.blue_offset = 20;
    raw.blue_length = 10;
    raw.alpha_offset = 30;
    raw.alpha_length = 2;
    assert_eq!(raw.to_buffered_image()?.rgba, [0, 0, 0, 170]);
    raw.red_length = 5;
    raw.green_offset = 5;
    raw.green_length = 6;
    raw.blue_offset = 11;
    raw.blue_length = 5;
    raw.alpha_length = 0;
    raw.buffer = (15u32 | 31 << 5 | 16 << 11).to_le_bytes().to_vec();
    assert_eq!(raw.to_buffered_image()?.rgba, [123, 125, 132, 255]);
    Ok(())
}

#[test]
fn rgb565_colors_are_little_endian_for_both_adapters_d63() -> Result<(), Box<dyn std::error::Error>> {
    let mut raw = packed_pixel(vec![0, 248, 224, 7, 31, 0, 255, 255]);
    raw.bits_per_pixel = 16;
    raw.width = 4;
    raw.red_offset = 11;
    raw.red_length = 5;
    raw.green_offset = 5;
    raw.green_length = 6;
    raw.blue_offset = 0;
    raw.blue_length = 5;
    raw.alpha_offset = 0;
    raw.alpha_length = 0;
    let expected = [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255];
    for color in [None, Some(ColorSpace::Srgb), Some(ColorSpace::Unknown)] {
        raw.color_space = color;
        assert_eq!(raw.to_buffered_image()?.rgba, expected);
        assert_eq!(
            BufferedImageScreenCaptureAdapter.decode(&raw)?,
            raw.to_buffered_image()?
        );
    }
    raw.color_space = Some(ColorSpace::P3);
    assert_eq!(
        BufferedImageScreenCaptureAdapter.decode(&raw)?,
        raw.to_buffered_image()?
    );
    Ok(())
}

#[test]
fn malformed_decode_inputs_return_errors_d63() {
    let valid = packed_pixel(vec![90, 130, 170, 255]);
    for case in 0..10 {
        let mut raw = valid.clone();
        match case {
            0 => raw.width = 0,
            1 => raw.height = u32::MAX,
            2 => raw.size = 3,
            3 => {
                raw.buffer.pop();
            }
            4 => raw.red_offset = 32,
            5 => raw.red_length = 0,
            6 => raw.red_length = 11,
            7 => {
                raw.blue_offset = 30;
                raw.blue_length = 8;
            }
            8 => raw.bits_per_pixel = 24,
            _ => {
                raw.width = u32::MAX;
                raw.height = u32::MAX;
            }
        }
        assert!(raw.to_buffered_image().is_err(), "case {case}");
        assert!(BufferedImageScreenCaptureAdapter.decode(&raw).is_err(), "case {case}");
    }
}

async fn capture_sixteen_bit_header(
    version: u32,
    channels: [u32; 8],
    pixels: Vec<u8>,
) -> Result<RawImage, Box<dyn std::error::Error>> {
    let mut header = vec![version];
    if version == 16 {
        header.extend([pixels.len() as u32, (pixels.len() / 2) as u32, 1]);
    } else {
        header.push(16);
        if version == 2 {
            header.push(0);
        }
        header.extend([pixels.len() as u32, (pixels.len() / 2) as u32, 1]);
        header.extend(channels);
    }
    let bytes: Vec<u8> = header.into_iter().flat_map(u32::to_le_bytes).collect();
    let session = stub::session(move |mut socket| async move {
        socket.expect_cmd("framebuffer:").await;
        socket.accept().await;
        socket.respond(&bytes).await;
        socket.expect_bytes(&[0]).await;
        socket.respond(&pixels).await;
    })
    .await;
    let result = session.client().execute(ScreenCaptureRequest, None).await;
    session.finish().await;
    Ok(result?)
}

#[tokio::test]
async fn bgr565_header_decodes_blue_u191() -> Result<(), Box<dyn std::error::Error>> {
    for version in [1, 2] {
        // Header order is R, B, G, A. 0xf800 is blue in this BGR565 layout.
        let raw = capture_sixteen_bit_header(version, [0, 5, 11, 5, 5, 6, 0, 0], vec![0, 248]).await?;
        assert_eq!(raw.get_argb(0)?, 0xff0000ff);
        let image = raw.to_buffered_image()?;
        assert_eq!(image.rgba, [0, 0, 255, 255]);
        assert_eq!(image, BufferedImageScreenCaptureAdapter.decode(&raw)?);
    }
    Ok(())
}

#[tokio::test]
async fn impossible_sixteen_bit_channel_headers_are_rejected_u191() -> Result<(), Box<dyn std::error::Error>> {
    for version in [1, 2] {
        for channels in [
            [16, 5, 0, 5, 5, 6, 0, 0],
            [12, 5, 0, 5, 5, 6, 0, 0],
            [u32::MAX, 5, 0, 5, 5, 6, 0, 0],
            [11, 5, 0, 5, 5, 6, 15, 2],
        ] {
            let raw = capture_sixteen_bit_header(version, channels, vec![0, 248]).await?;
            assert_eq!(raw.get_argb(0), Err(malformed()));
            assert_eq!(raw.to_buffered_image(), Err(malformed()));
            assert_eq!(BufferedImageScreenCaptureAdapter.decode(&raw), Err(malformed()));
        }
        for length in [0, 11, u32::MAX] {
            let raw = capture_sixteen_bit_header(version, [0, length, 11, 5, 5, 6, 0, 0], vec![0, 248]).await?;
            let expected = Err(AdbError::Runtime(format!("Unexpected mask length {length}")));
            assert_eq!(raw.get_argb(0), expected);
            assert!(raw.to_buffered_image().is_err());
            assert!(BufferedImageScreenCaptureAdapter.decode(&raw).is_err());
        }
    }
    Ok(())
}

#[tokio::test]
async fn sixteen_bit_layouts_keep_floor_scaling_alpha_and_png_bytes_u191() -> Result<(), Box<dyn std::error::Error>> {
    // Midtones separate floor from nearest rounding, unlike the old all-black fixture.
    let pixels: Vec<u8> = [0xf800u16, 0x07e0, 0x001f, 0xffff, 0x7bf0, 0x0821]
        .into_iter()
        .flat_map(u16::to_le_bytes)
        .collect();
    for (channels, expected, hash) in [
        (
            [11, 5, 0, 5, 5, 6, 0, 0],
            vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255, 123, 125, 131, 255, 8, 4, 8, 255,
            ],
            "33f55ca5933ff22db670c9515b3bdcb50ca46b311242ae55a9e22daa6d634076",
        ),
        (
            [0, 5, 11, 5, 5, 6, 0, 0],
            vec![
                0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255, 255, 255, 255, 255, 131, 125, 123, 255, 8, 4, 8, 255,
            ],
            "65bd58c1c74b210dd51d5f688776940f23b3bb55270d4447246654a6b8c22cfc",
        ),
    ] {
        for version in [1, 2] {
            let raw = capture_sixteen_bit_header(version, channels, pixels.clone()).await?;
            let image = raw.to_buffered_image()?;
            assert_eq!(image.rgba, expected);
            assert_eq!(image, BufferedImageScreenCaptureAdapter.decode(&raw)?);
            assert_eq!(format!("{:x}", Sha256::digest(image.encode_png()?)), hash);
        }
    }
    // ARGB4444 proves a declared alpha channel is read, rather than forced opaque.
    let raw = capture_sixteen_bit_header(2, [8, 4, 0, 4, 4, 4, 12, 4], vec![0x23, 0x81]).await?;
    assert_eq!(raw.to_buffered_image()?.rgba, [17, 34, 51, 136]);
    Ok(())
}

#[tokio::test]
async fn legacy_layoutless_images_remain_rgb565_u191() -> Result<(), Box<dyn std::error::Error>> {
    let legacy = capture_sixteen_bit_header(16, [0; 8], vec![0, 248]).await?;
    assert_eq!(
        (
            legacy.red_offset,
            legacy.red_length,
            legacy.green_offset,
            legacy.green_length,
            legacy.blue_offset,
            legacy.blue_length,
            legacy.alpha_length
        ),
        (11, 5, 5, 6, 0, 5, 0)
    );
    assert_eq!(legacy.to_buffered_image()?.rgba, [255, 0, 0, 255]);
    let mut version_zero = legacy;
    version_zero.version = 0;
    version_zero.red_offset = 0;
    version_zero.red_length = 0;
    version_zero.green_offset = 0;
    version_zero.green_length = 0;
    version_zero.blue_offset = 0;
    version_zero.blue_length = 0;
    assert_eq!(version_zero.to_buffered_image()?.rgba, [255, 0, 0, 255]);
    assert_eq!(
        version_zero.to_buffered_image()?,
        BufferedImageScreenCaptureAdapter.decode(&version_zero)?
    );
    let session = stub::session(|mut socket| async move {
        socket.expect_cmd("framebuffer:").await;
        socket.accept().await;
        socket.respond(&0u32.to_le_bytes()).await;
    })
    .await;
    let result = session.client().execute(ScreenCaptureRequest, None).await;
    session.finish().await;
    assert_eq!(
        result,
        Err(AdbError::UnsupportedImageProtocol("Unsupported version 0".into()))
    );
    Ok(())
}
