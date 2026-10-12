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
    let mut out = alloc::vec::Vec::from(HEADER_LE);
    value.serialize(&mut ser::Serializer::new(&mut out))?;
    Ok(out)
}

/// Decodes a value from an encapsulated payload.
///
/// The byte order comes from the header (`CDR_LE` or `CDR_BE`); any other
/// representation is [`Error::InvalidHeader`]. Trailing bytes are ignored.
pub fn from_bytes<'de, T>(bytes: &'de [u8]) -> Result<T>
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
    T::deserialize(&mut de::Deserializer::new(body, big_endian))
}
