#![doc = include_str!("../README.md")]
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]
#![no_std]

#[cfg(feature = "alloc")]
extern crate alloc;

mod de;
mod error;
mod ser;

pub use error::{Error, Result};

/// Encapsulation header for little-endian XCDR1 (`CDR_LE`), the only one this
/// crate writes.
pub const HEADER_LE: [u8; 4] = [0x00, 0x01, 0x00, 0x00];

const HEADER_LEN: usize = HEADER_LE.len();

/// Encodes `value` with its header into `out`, returning the bytes written.
///
/// Fails with [`Error::BufferTooSmall`] if `out` cannot hold it; `out` is then
/// partially written.
pub fn to_slice<T>(value: &T, out: &mut [u8]) -> Result<usize>
where
    T: serde::Serialize + ?Sized,
{
    let (header, body) = out
        .split_at_mut_checked(HEADER_LEN)
        .ok_or(Error::BufferTooSmall)?;
    header.copy_from_slice(&HEADER_LE);
    let mut serializer = ser::Serializer::new(ser::SliceWriter::new(body));
    value.serialize(&mut serializer)?;
    Ok(HEADER_LEN + serializer.position())
}

/// Encodes `value` with its header into a new `Vec`.
#[cfg(feature = "alloc")]
pub fn to_vec<T>(value: &T) -> Result<alloc::vec::Vec<u8>>
where
    T: serde::Serialize + ?Sized,
{
    let mut out = alloc::vec::Vec::new();
    append_to_vec(value, &mut out)?;
    Ok(out)
}

/// Appends `value` with its header after the existing contents of `out`.
///
/// Alignment is measured from the end of the appended header, so a prefix
/// already in `out` (such as a transport or envelope header) does not shift
/// it. On error `out` may hold a partial encoding after its original contents.
#[cfg(feature = "alloc")]
pub fn append_to_vec<T>(value: &T, out: &mut alloc::vec::Vec<u8>) -> Result<()>
where
    T: serde::Serialize + ?Sized,
{
    out.extend_from_slice(&HEADER_LE);
    value.serialize(&mut ser::Serializer::new(out))
}

/// Decodes a value from an encapsulated payload.
///
/// The byte order comes from the header (`CDR_LE` or `CDR_BE`); any other
/// representation is [`Error::InvalidHeader`]. Trailing bytes are ignored.
pub fn from_bytes<'de, T>(bytes: &'de [u8]) -> Result<T>
where
    T: serde::Deserialize<'de>,
{
    decode(bytes).map(|(value, _, _)| value)
}

/// Decodes one encapsulated value and returns its consumed byte count.
///
/// The count includes the four-byte encapsulation header, internal alignment
/// padding, and the trailing padding the header declares: XCDR stores that
/// count (0–3) in the two low bits of the options field, and DDS writers use it
/// to pad a payload to a 4-byte boundary. Any further trailing bytes are not
/// counted. Declared padding the input does not contain is
/// [`Error::UnexpectedEof`]; padding contents are not checked. Alignment starts
/// at the body, just as in [`from_bytes`]. This also supports borrowed payload
/// fields without alloc.
pub fn from_bytes_with_consumed<'de, T>(bytes: &'de [u8]) -> Result<(T, usize)>
where
    T: serde::Deserialize<'de>,
{
    let (value, end, padding) = decode(bytes)?;
    let used = end + padding;
    if used > bytes.len() {
        return Err(Error::UnexpectedEof);
    }
    Ok((value, used))
}

/// The value, the end of its encoding, and the padding the header declares.
fn decode<'de, T>(bytes: &'de [u8]) -> Result<(T, usize, usize)>
where
    T: serde::Deserialize<'de>,
{
    let (header, body) = bytes
        .split_at_checked(HEADER_LEN)
        .ok_or(Error::UnexpectedEof)?;
    let big_endian = match [header[0], header[1]] {
        [0x00, 0x01] => false,
        [0x00, 0x00] => true,
        other => return Err(Error::InvalidHeader(other)),
    };
    // Options are a big-endian u16 in header[2..4]; padding is its low two bits.
    let padding = usize::from(header[3] & 0b11);
    let mut deserializer = de::Deserializer::new(body, big_endian);
    let value = T::deserialize(&mut deserializer)?;
    Ok((value, HEADER_LEN + deserializer.position(), padding))
}
