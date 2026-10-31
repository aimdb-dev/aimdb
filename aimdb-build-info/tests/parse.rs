//! The section reader rules, against sections built by hand here,
//! independent of any encoder in the crate.

use aimdb_build_info::{
    parse, ParseError, PostLink, ERASED, FORMAT, HEADER_LEN, MAGIC, SECTION_MAX,
};

/// A format-1 section: `header_len` header bytes, `payload`, zero padding to
/// the next multiple of 8, then an erased region of `region_len` bytes.
fn section(header_len: usize, payload: &[u8], region_len: usize) -> Vec<u8> {
    let region_off = (header_len + payload.len()).div_ceil(8) * 8;
    let total_len = region_off + region_len;
    let mut b = vec![0u8; total_len];
    b[..8].copy_from_slice(&MAGIC);
    b[8..10].copy_from_slice(&FORMAT.to_le_bytes());
    b[10..12].copy_from_slice(&(header_len as u16).to_le_bytes());
    b[12..16].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    b[16..20].copy_from_slice(&(region_off as u32).to_le_bytes());
    b[20..24].copy_from_slice(&(region_len as u32).to_le_bytes());
    b[24..28].copy_from_slice(&(total_len as u32).to_le_bytes());
    b[header_len..header_len + payload.len()].copy_from_slice(payload);
    b[region_off..].fill(ERASED);
    b
}

fn put_u32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// Offset of the region in a section built by [`section`].
fn region_off(b: &[u8]) -> usize {
    u32::from_le_bytes(b[16..20].try_into().unwrap()) as usize
}

const PAYLOAD: &[u8] = br#"{"schema":1,"name":"app"}"#;

#[test]
fn round_trip() {
    let b = section(HEADER_LEN, PAYLOAD, 64);
    let s = parse(&b).unwrap();
    assert_eq!(s.format, 1);
    assert_eq!(s.payload, PAYLOAD);
    assert_eq!(s.region.len(), 64);
    assert_eq!(s.post_link(), PostLink::Erased);
}

#[test]
fn bytes_past_the_section_are_ignored() {
    let mut b = section(HEADER_LEN, PAYLOAD, 16);
    let len = b.len();
    b.extend_from_slice(&[0xA5; 100]);
    let s = parse(&b).unwrap();
    assert_eq!(s.payload, PAYLOAD);
    assert_eq!(s.region.len(), 16);
    assert_eq!(s.region.as_ptr_range().end, b[..len].as_ptr_range().end);
}

#[test]
fn longer_header_is_skipped_and_payload_read_from_header_len() {
    let mut b = section(48, PAYLOAD, 8);
    b[32..48].fill(0x5A); // a future header field
    assert_eq!(parse(&b).unwrap().payload, PAYLOAD);
}

#[test]
fn wrong_magic() {
    let mut b = section(HEADER_LEN, PAYLOAD, 8);
    b[0] = b'X';
    assert_eq!(parse(&b), Err(ParseError::NoMagic));
    assert_eq!(parse(&b"AIMDB-B"[..]), Err(ParseError::NoMagic));
    assert_eq!(parse(&[]), Err(ParseError::NoMagic));
}

#[test]
fn unknown_format() {
    for format in [0u16, 2, u16::MAX] {
        let mut b = section(HEADER_LEN, PAYLOAD, 8);
        b[8..10].copy_from_slice(&format.to_le_bytes());
        assert_eq!(parse(&b), Err(ParseError::UnknownFormat(format)));
    }
}

#[test]
fn header_shorter_than_32() {
    let mut b = section(HEADER_LEN, PAYLOAD, 8);
    b[10..12].copy_from_slice(&31u16.to_le_bytes());
    assert_eq!(parse(&b), Err(ParseError::Inconsistent));
}

#[test]
fn payload_past_region_off() {
    let mut b = section(HEADER_LEN, PAYLOAD, 8);
    let off = region_off(&b) as u32;
    put_u32(&mut b, 12, off - HEADER_LEN as u32 + 1);
    assert_eq!(parse(&b), Err(ParseError::Inconsistent));
    // Exactly up to region_off is fine.
    put_u32(&mut b, 12, off - HEADER_LEN as u32);
    assert!(parse(&b).is_ok());
}

#[test]
fn unaligned_region_off() {
    let mut b = section(HEADER_LEN, PAYLOAD, 8);
    let off = region_off(&b) as u32;
    put_u32(&mut b, 16, off + 4);
    put_u32(&mut b, 24, off + 4 + 8);
    assert_eq!(parse(&b), Err(ParseError::Inconsistent));
}

#[test]
fn total_len_disagrees() {
    let mut b = section(HEADER_LEN, PAYLOAD, 8);
    let total = b.len() as u32;
    put_u32(&mut b, 24, total - 8);
    assert_eq!(parse(&b), Err(ParseError::Inconsistent));
}

#[test]
fn section_max() {
    let fixed = (HEADER_LEN + PAYLOAD.len()).div_ceil(8) * 8;
    let b = section(HEADER_LEN, PAYLOAD, SECTION_MAX - fixed);
    assert_eq!(b.len(), SECTION_MAX);
    assert!(parse(&b).is_ok());
    let b = section(HEADER_LEN, PAYLOAD, SECTION_MAX - fixed + 1);
    assert_eq!(parse(&b), Err(ParseError::Inconsistent));
}

#[test]
fn fewer_bytes_than_total_len() {
    let b = section(HEADER_LEN, PAYLOAD, 8);
    assert_eq!(parse(&b[..b.len() - 1]), Err(ParseError::Truncated));
    assert_eq!(parse(&b[..HEADER_LEN - 1]), Err(ParseError::Truncated));
}

#[test]
fn huge_header_values_are_rejected_not_wrapped() {
    let mut b = section(HEADER_LEN, PAYLOAD, 8);
    for at in [12, 16, 20, 24] {
        put_u32(&mut b, at, !7); // u32::MAX, rounded down to a multiple of 8
    }
    assert!(parse(&b).is_err());
}

#[test]
fn region_absent() {
    let b = section(HEADER_LEN, PAYLOAD, 0);
    let s = parse(&b).unwrap();
    assert!(s.region.is_empty());
    assert_eq!(s.post_link(), PostLink::None);
}

#[test]
fn region_written() {
    let mut b = section(HEADER_LEN, PAYLOAD, 16);
    let off = region_off(&b);
    put_u32(&mut b, off, 3);
    b[off + 4..off + 7].copy_from_slice(b"xyz");
    assert_eq!(parse(&b).unwrap().post_link(), PostLink::Written(b"xyz"));

    // Filling the region exactly, and writing nothing.
    put_u32(&mut b, off, 12);
    assert_eq!(
        parse(&b).unwrap().post_link(),
        PostLink::Written(&b[off + 4..off + 16])
    );
    put_u32(&mut b, off, 0);
    assert_eq!(parse(&b).unwrap().post_link(), PostLink::Written(&[]));
}

#[test]
fn region_malformed() {
    let mut b = section(HEADER_LEN, PAYLOAD, 16);
    let off = region_off(&b);
    for len in [13, u32::MAX - 3, u32::MAX - 1] {
        put_u32(&mut b, off, len);
        assert_eq!(parse(&b).unwrap().post_link(), PostLink::Malformed, "{len}");
    }
    // Only three bytes erased: a length, and far too long.
    b[off..off + 3].fill(ERASED);
    b[off + 3] = 0;
    assert_eq!(parse(&b).unwrap().post_link(), PostLink::Malformed);
}

#[test]
fn regions_of_one_to_three_bytes_are_malformed() {
    for len in 1..4 {
        let b = section(HEADER_LEN, PAYLOAD, len);
        assert_eq!(parse(&b).unwrap().post_link(), PostLink::Malformed, "{len}");
    }
}

#[test]
fn no_panic_on_any_truncation_or_bit_flip() {
    let mut good = section(48, PAYLOAD, 24);
    let off = region_off(&good);
    put_u32(&mut good, off, 5);
    for n in 0..=good.len() {
        let _ = parse(&good[..n]).map(|s| s.post_link());
    }
    for i in 0..good.len() {
        for bit in 0..8 {
            let mut b = good.clone();
            b[i] ^= 1 << bit;
            let _ = parse(&b).map(|s| s.post_link());
        }
    }
}

#[test]
fn single_line() {
    assert!(aimdb_build_info::payload_is_single_line(
        br#"{"a": "x\ny"}"#
    ));
    assert!(!aimdb_build_info::payload_is_single_line(b"{\n}"));
    assert!(!aimdb_build_info::payload_is_single_line(b"{\r}"));
}
