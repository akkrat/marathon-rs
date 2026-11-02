//! WP 2.3b frame-decoder tests (PLAN §3.3). The decoder has no Kotlin
//! tests and no parity rows (orchestrator decision 2); these are new Rust
//! tests named by rule, derived from `AppleSimulatorProvider.readSocket`.
//! `readInt` big-endianness and `decodeString` malformed-UTF-8 replacement
//! are JVM-probed in `fixtures/oracle/bundle-probe.jsonl` (`ktor-facts`);
//! the close behaviour over a real ktor socket in
//! `fixtures/oracle/frame-eof-probe.jsonl`.

use jcompat::{file_manager::JavaPath, jtime::instant_of, writers::JavaString};
use marathon_config::{
    MarathonDevices,
    binding::{BindingContext, Profile},
    dynamic::{FrameDecoder, FrameError, FrameSegment, MAX_FRAME_PAYLOAD},
};

fn context() -> BindingContext {
    BindingContext {
        profile: Profile::Marathondevices,
        marathonfile: JavaPath::from_text("/Marathonfile"),
        marathonfile_dir: JavaPath::from_text("/"),
        process_dir: JavaPath::from_text("/"),
        environment: vec![(JavaString::from("ADDR"), JavaString::from("node-7"))],
        now: instant_of(0, 0).unwrap(),
        analytics_tracking: false,
        bugsnag_reporting: false,
    }
}

fn frame(payload: &[u8]) -> Vec<u8> {
    frame_with_length(i32::try_from(payload.len()).unwrap(), payload)
}

fn frame_with_length(length: i32, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0xde, 0xad, 0xbe, 0xef];
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

const ONE: &[u8] = b"workers: [{transport: {type: local}, devices: [{type: host}]}]";
const TWO: &[u8] = b"workers: [{transport: {type: ssh, addr: ${ADDR}}, devices: [{type: simulator, udid: U}]}]";

/// What a close on a frame boundary yields: Kotlin's reader throws there
/// too (`readFully` of the next reserved bytes), and the producer stops.
fn assert_boundary_close(decoder: &mut FrameDecoder) {
    match decoder.finish() {
        Some(Err(
            error @ FrameError::Truncated {
                segment: FrameSegment::Reserved,
                expected: 4,
                received: 0,
            },
        )) => {
            assert_eq!(error.message(), "Unexpected EOF: expected 4 more bytes");
        }
        other => panic!("expected a boundary close, got {other:?}"),
    }
    assert!(decoder.finish().is_none(), "the close is reported once");
}

fn snapshots(outcomes: Vec<std::result::Result<MarathonDevices, FrameError>>) -> Vec<MarathonDevices> {
    outcomes
        .into_iter()
        .map(|o| o.unwrap_or_else(|e| panic!("frame failed: {e:?}")))
        .collect()
}

#[test]
fn split_at_every_byte_boundary_of_a_two_frame_stream() {
    let stream = [frame(ONE), frame(TWO)].concat();
    let ctx = context();
    let whole = snapshots(FrameDecoder::new().feed(&stream, &ctx));
    assert_eq!(whole.len(), 2);
    // The substitution inside the second payload ran.
    assert_eq!(whole[1].workers[0].transport.id().to_string_lossy(), "node-7:22");
    for split in 0..=stream.len() {
        let mut decoder = FrameDecoder::new();
        let mut got = decoder.feed(&stream[..split], &ctx);
        got.extend(decoder.feed(&stream[split..], &ctx));
        assert_eq!(snapshots(got), whole, "split at {split}");
        assert_boundary_close(&mut decoder);
    }
    // One byte at a time, and coalesced in a single feed.
    let mut decoder = FrameDecoder::new();
    let mut got = Vec::new();
    for byte in &stream {
        got.extend(decoder.feed(std::slice::from_ref(byte), &ctx));
    }
    assert_eq!(snapshots(got), whole);
}

#[test]
fn zero_length_is_a_binding_failure() {
    let ctx = context();
    let mut decoder = FrameDecoder::new();
    let out = decoder.feed(&frame(b""), &ctx);
    assert_eq!(out.len(), 1);
    assert!(matches!(out[0], Err(FrameError::Binding(_))), "{:?}", out[0]);
    assert!(decoder.is_failed());
    // After a failure the decoder is dead (the producer stops, A1).
    assert!(decoder.feed(&frame(ONE), &ctx).is_empty());
    assert!(decoder.finish().is_none());
}

#[test]
fn negative_and_oversized_lengths() {
    let ctx = context();
    let out = FrameDecoder::new().feed(&frame_with_length(-1, b""), &ctx);
    assert!(matches!(out[0], Err(FrameError::NegativeLength { length: -1 })));
    let err = match &out[0] {
        Err(e) => e,
        Ok(_) => unreachable!(),
    };
    assert_eq!(err.java_class(), Some("java.lang.IllegalArgumentException"));

    let out = FrameDecoder::new().feed(&frame_with_length(MAX_FRAME_PAYLOAD + 1, b""), &ctx);
    assert!(matches!(out[0], Err(FrameError::TooLong { .. })));
    let err = match &out[0] {
        Err(e) => e,
        Ok(_) => unreachable!(),
    };
    assert_eq!(err.message(), "Device provider update too long: maximum is 2^20 bytes");
    assert_eq!(err.java_class(), Some("java.lang.RuntimeException"));
}

#[test]
fn max_length_payload_is_accepted_then_a_small_frame() {
    let ctx = context();
    // A valid document of exactly 2^20 bytes: the one-liner padded with a
    // YAML comment.
    let mut payload = b"workers: [] ".to_vec();
    payload.push(b'#');
    payload.resize(MAX_FRAME_PAYLOAD as usize, b'x');
    assert_eq!(payload.len(), MAX_FRAME_PAYLOAD as usize);
    let stream = [frame(&payload), frame(ONE)].concat();
    let mut decoder = FrameDecoder::new();
    let mut got = Vec::new();
    for chunk in stream.chunks(65_536) {
        got.extend(decoder.feed(chunk, &ctx));
    }
    let got = snapshots(got);
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].workers.len(), 0);
    assert_eq!(got[1].workers.len(), 1);
    assert_boundary_close(&mut decoder);
}

#[test]
fn eof_inside_each_segment() {
    let ctx = context();
    let cases: Vec<(Vec<u8>, FrameSegment)> = vec![
        (vec![1, 2], FrameSegment::Reserved),
        (vec![1, 2, 3, 4, 0], FrameSegment::Length),
        (frame(ONE)[..frame(ONE).len() - 3].to_vec(), FrameSegment::Payload),
    ];
    for (bytes, segment) in cases {
        let mut decoder = FrameDecoder::new();
        assert!(decoder.feed(&bytes, &ctx).is_empty());
        let finish = decoder.finish().expect("truncated");
        assert!(
            matches!(finish, Err(FrameError::Truncated { segment: got, .. }) if got == segment),
            "{finish:?}"
        );
    }
    // A close on a frame boundary is a failure too, before any frame and
    // after one.
    assert_boundary_close(&mut FrameDecoder::new());
    let mut decoder = FrameDecoder::new();
    decoder.feed(&frame(ONE), &ctx);
    assert_boundary_close(&mut decoder);
}

/// `fixtures/oracle/frame-eof-probe.jsonl`: the read half of `readSocket`
/// over a real ktor loopback socket. The probe reads frames without
/// binding them; its complete payloads are valid documents, so the frame
/// count must match, except for the zero-length frame, which Kotlin reads
/// and then fails to bind (`zero_length_is_a_binding_failure`).
#[test]
fn frame_eof_probe_matches_jvm() {
    let table = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/oracle/frame-eof-probe.jsonl"
    ))
    .unwrap();
    let ctx = context();
    let mut checked = 0;
    for line in table.lines().skip(1) {
        let row: serde_json::Value = serde_json::from_str(line).unwrap();
        let id = row["id"].as_str().unwrap();
        let hex = row["bytes"].as_str().unwrap();
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        let expected_frames = row["result"]["frames"].as_array().unwrap().len();
        let expected = &row["result"]["error"];
        for chunk in [bytes.len().max(1), 1] {
            let mut decoder = FrameDecoder::new();
            let mut outcomes = Vec::new();
            for piece in bytes.chunks(chunk) {
                outcomes.extend(decoder.feed(piece, &ctx));
            }
            outcomes.extend(decoder.finish());
            let error = outcomes.pop().expect("every close fails").expect_err(id);
            assert!(outcomes.iter().all(Result::is_ok), "{id}: {outcomes:?}");
            if id == "zero-length-then-boundary" {
                assert!(matches!(error, FrameError::Binding(_)), "{id}: {error:?}");
                assert!(outcomes.is_empty());
                continue;
            }
            assert_eq!(outcomes.len(), expected_frames, "{id} frames");
            assert_eq!(error.java_class(), expected["class"].as_str(), "{id} class");
            assert_eq!(error.message(), expected["message"].as_str().unwrap(), "{id} message");
        }
        checked += 1;
    }
    assert_eq!(checked, 16);
}

#[test]
fn binding_and_substitution_failures_keep_their_kinds() {
    let ctx = context();
    // Invalid YAML payload: a binding failure carrying its class.
    let out = FrameDecoder::new().feed(&frame(b"workers: ["), &ctx);
    match &out[0] {
        Err(FrameError::Binding(error)) => {
            assert_eq!(error.stage, marathon_config::binding::ValidationStage::Provider);
        }
        other => panic!("expected Binding, got {other:?}"),
    }
    // A null payload document is its own kind, not an empty snapshot.
    let out = FrameDecoder::new().feed(&frame(b"null"), &ctx);
    assert!(matches!(out[0], Err(FrameError::NullDocument)), "{:?}", out[0]);
    // Malformed UTF-8 is replaced (ktor decodeString, probed), then the
    // replaced text fails binding rather than the decode.
    let out = FrameDecoder::new().feed(&frame(&[0x61, 0x80, 0xc3]), &ctx);
    assert!(matches!(out[0], Err(FrameError::Binding(_))), "{:?}", out[0]);
    // A substitution cycle escapes as its own kind (unwrapped in Kotlin).
    let mut cyclic = context();
    cyclic.environment = vec![(JavaString::from("ADDR"), JavaString::from("${ADDR}"))];
    let out = FrameDecoder::new().feed(&frame(TWO), &cyclic);
    assert!(matches!(out[0], Err(FrameError::Substitution(_))), "{:?}", out[0]);
}
