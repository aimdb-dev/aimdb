//! COBS framing round-trips regardless of how the byte stream is chunked — the
//! property a real UART needs (a `read()` returns an arbitrary slice of the wire).
//!
//! Transport-agnostic: exercises only [`framing`](aimdb_serial_connector::framing),
//! so it runs under the crate's default features (no runtime needed).

use aimdb_serial_connector::framing::{encode_frame, FrameAccumulator, DELIM};

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
