use aimdb_tcp_connector::framing::{encode_frame, FrameAccumulator, FrameError};

fn drain(acc: &mut FrameAccumulator) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(frame) = acc.next_frame() {
        out.push(frame.expect("valid frame"));
    }
    out
}

#[test]
fn single_frame_roundtrips() {
    let payload = br#"{"t":"req","id":1,"method":"record.list"}"#;
    let mut wire = Vec::new();
    encode_frame(payload, &mut wire).expect("encode");

    let mut acc = FrameAccumulator::new();
    acc.push_bytes(&wire);
    assert_eq!(drain(&mut acc), vec![payload.to_vec()]);
}

#[test]
fn survives_byte_by_byte_delivery() {
    let payload = b"hello";
    let mut wire = Vec::new();
    encode_frame(payload, &mut wire).expect("encode");

    let mut acc = FrameAccumulator::new();
    for b in wire {
        acc.push_bytes(&[b]);
    }
    assert_eq!(drain(&mut acc), vec![payload.to_vec()]);
}

#[test]
fn many_frames_across_arbitrary_chunks() {
    let payloads = [b"one".as_slice(), b"two".as_slice(), b"three".as_slice()];
    let mut wire = Vec::new();
    for payload in payloads {
        encode_frame(payload, &mut wire).expect("encode");
    }

    for chunk in 1..wire.len() {
        let mut acc = FrameAccumulator::new();
        for part in wire.chunks(chunk) {
            acc.push_bytes(part);
        }
        assert_eq!(
            drain(&mut acc),
            payloads.iter().map(|p| p.to_vec()).collect::<Vec<_>>()
        );
    }
}

#[test]
fn waits_for_partial_header_and_payload() {
    let mut wire = Vec::new();
    encode_frame(b"abcdef", &mut wire).expect("encode");

    let mut acc = FrameAccumulator::new();
    acc.push_bytes(&wire[..2]);
    assert!(acc.next_frame().is_none());
    acc.push_bytes(&wire[2..7]);
    assert!(acc.next_frame().is_none());
    acc.push_bytes(&wire[7..]);
    assert_eq!(acc.next_frame().unwrap().unwrap(), b"abcdef");
}

#[test]
fn rejects_oversized_frame_before_payload_arrives() {
    let mut acc = FrameAccumulator::with_max_frame(4);
    acc.push_bytes(&5u32.to_be_bytes());
    assert_eq!(acc.next_frame(), Some(Err(FrameError::TooLarge)));
}

#[test]
fn empty_payload_roundtrips() {
    let mut wire = Vec::new();
    encode_frame(b"", &mut wire).expect("encode");

    let mut acc = FrameAccumulator::new();
    acc.push_bytes(&wire);
    assert_eq!(acc.next_frame().unwrap().unwrap(), b"");
}
