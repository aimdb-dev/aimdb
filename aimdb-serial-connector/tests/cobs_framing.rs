//! COBS framing round-trips regardless of how the byte stream is chunked — the
//! property a real UART needs (a `read()` returns an arbitrary slice of the wire).
//!
//! Transport-agnostic: exercises only [`framing`](aimdb_serial_connector::framing),
//! so it runs under the crate's default features (no runtime needed).

use aimdb_serial_connector::framing::{encode_frame, FrameAccumulator, FrameError, DELIM};

/// Encode several payloads into one byte stream, then feed that stream to a fresh
/// accumulator in `chunk`-sized slices and collect every frame it yields.
fn roundtrip_in_chunks(payloads: &[&[u8]], chunk: usize) -> Vec<Vec<u8>> {
    let mut wire = Vec::new();
    for p in payloads {
        encode_frame(p, &mut wire);
    }

    let mut acc = FrameAccumulator::new();
    let mut out = Vec::new();
    for bytes in wire.chunks(chunk.max(1)) {
        acc.push_bytes(bytes);
        while let Some(frame) = acc.next_frame() {
            out.push(frame.expect("valid COBS frame"));
        }
    }
    out
}

#[test]
fn single_frame_roundtrips() {
    let payload = br#"{"t":"req","id":1,"method":"record.list"}"#;
    let frames = roundtrip_in_chunks(&[payload], usize::MAX);
    assert_eq!(frames, vec![payload.to_vec()]);
}

#[test]
fn survives_byte_by_byte_delivery() {
    // The pathological case: the receiver sees one byte at a time, so a frame is
    // only complete on the read that delivers the sentinel.
    let payload = br#"{"level":42,"nested":{"a":[1,2,3]}}"#;
    let frames = roundtrip_in_chunks(&[payload], 1);
    assert_eq!(frames, vec![payload.to_vec()]);
}

#[test]
fn many_frames_across_arbitrary_chunk_boundaries() {
    let payloads: &[&[u8]] = &[b"a", b"bb", b"ccc", br#"{"k":"v"}"#, b"\x01\x02\x03"];
    // Every chunk size must reassemble the exact same frame sequence.
    for chunk in [1usize, 2, 3, 5, 7, 64, usize::MAX] {
        let frames = roundtrip_in_chunks(payloads, chunk);
        let expected: Vec<Vec<u8>> = payloads.iter().map(|p| p.to_vec()).collect();
        assert_eq!(frames, expected, "chunk size {chunk}");
    }
}

#[test]
fn empty_payload_roundtrips() {
    let frames = roundtrip_in_chunks(&[b""], usize::MAX);
    assert_eq!(frames, vec![Vec::<u8>::new()]);
}

#[test]
fn encoded_frame_never_contains_the_sentinel_except_the_terminator() {
    let mut wire = Vec::new();
    encode_frame(br#"{"any":"json payload"}"#, &mut wire);
    assert_eq!(wire.last(), Some(&DELIM), "frame ends with the sentinel");
    assert_eq!(
        wire[..wire.len() - 1]
            .iter()
            .filter(|&&b| b == DELIM)
            .count(),
        0,
        "the COBS body is free of the sentinel byte"
    );
}

#[test]
fn caps_buffered_bytes_and_resyncs_after_overflow() {
    // A stream that never delimits must not buffer without bound (an OOM on a small
    // embedded heap): once an un-delimited run exceeds the cap, the accumulator
    // drops it (one `FrameError`) and resyncs on the next sentinel.
    let mut acc = FrameAccumulator::with_max_frame(16);

    // 40 bytes of sentinel-free noise — no frame boundary in sight.
    acc.push_bytes(&[0x42; 40]);
    assert_eq!(
        acc.next_frame(),
        Some(Err(FrameError)),
        "overflow is reported"
    );
    // The oversized run was dropped, not retained for a retry.
    assert!(acc.next_frame().is_none());

    // The tail of the abandoned frame (more noise, then its terminating sentinel) is
    // skipped, and the next whole frame is recovered intact.
    acc.push_bytes(&[0x42; 5]);
    acc.push_bytes(&[DELIM]);
    let mut wire = Vec::new();
    encode_frame(b"recovered", &mut wire);
    acc.push_bytes(&wire);
    assert_eq!(
        acc.next_frame().expect("frame").expect("valid"),
        b"recovered"
    );
    assert!(acc.next_frame().is_none());
}

#[test]
fn frames_up_to_the_cap_still_pass() {
    // The cap only fires on an *un-delimited* run; a delimited frame at the cap size
    // round-trips normally.
    let mut acc = FrameAccumulator::with_max_frame(4096);
    let payload = vec![0x41u8; 512];
    let mut wire = Vec::new();
    encode_frame(&payload, &mut wire);
    acc.push_bytes(&wire);
    assert_eq!(acc.next_frame().expect("frame").expect("valid"), payload);
    assert!(acc.next_frame().is_none());
}

#[test]
fn resynchronizes_after_a_leading_sentinel() {
    // A receiver that joins mid-frame sees a stray sentinel first; it should be
    // skipped and the next whole frame recovered.
    let mut acc = FrameAccumulator::new();
    acc.push_bytes(&[DELIM]); // garbage tail of a frame we joined late
    assert!(acc.next_frame().is_none());

    let mut wire = Vec::new();
    encode_frame(b"hello", &mut wire);
    acc.push_bytes(&wire);
    assert_eq!(acc.next_frame().expect("frame").expect("valid"), b"hello");
    assert!(acc.next_frame().is_none());
}
