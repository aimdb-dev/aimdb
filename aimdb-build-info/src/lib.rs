#![doc = include_str!("../README.md")]
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]
#![no_std]

#[cfg(feature = "build")]
extern crate std;

#[cfg(feature = "build")]
mod build;
#[cfg(feature = "build")]
pub use build::{encode, BuildInfo, Error, DEFAULT_POST_LINK_LEN};

/// The section's first eight bytes.
pub const MAGIC: [u8; 8] = *b"AIMDB-BI";

/// The layout version this crate reads. It changes only when a field changes
/// meaning; new header fields go past byte 32 instead.
pub const FORMAT: u16 = 1;

/// Header length of format 1's initial layout, and the least a reader accepts.
pub const HEADER_LEN: usize = 32;

/// Largest section a reader accepts. A probe reads the header first and never
/// trusts a larger `total_len`.
pub const SECTION_MAX: usize = 64 * 1024;

/// Largest payload, in bytes. The build helper refuses to write more and
/// AimDB refuses to serve more, so every AimX reply that carries it fits the
/// smallest frame.
pub const PAYLOAD_MAX: usize = 4 * 1024;

/// The byte erased flash reads as. An unwritten post-link region holds only
/// this byte.
pub const ERASED: u8 = 0xFF;

/// The payload schema version this crate writes. It changes only when a
/// standard field changes type or meaning; readers ignore fields they do not
/// know.
pub const SCHEMA: u64 = 1;

/// File name of the section image in the build script's `OUT_DIR`.
pub const FILE_NAME: &str = "aimdb_build_info.bin";

/// Why bytes are not a build-info section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// No magic: there is no build info here.
    NoMagic,
    /// A layout version this crate does not read.
    UnknownFormat(u16),
    /// Fewer bytes than the header says the section has.
    Truncated,
    /// The header's lengths and offsets do not agree, or the section is
    /// larger than [`SECTION_MAX`].
    Inconsistent,
}

impl core::fmt::Display for ParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoMagic => f.write_str("no build-info magic"),
            Self::UnknownFormat(v) => write!(f, "unknown build-info format {v}"),
            Self::Truncated => f.write_str("build-info section is truncated"),
            Self::Inconsistent => f.write_str("build-info header is inconsistent"),
        }
    }
}

impl core::error::Error for ParseError {}

/// A parsed section, borrowing the bytes it was parsed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Section<'a> {
    /// Layout version.
    pub format: u16,
    /// The JSON payload, without its zero padding.
    pub payload: &'a [u8],
    /// The post-link region, `region_len` bytes; see [`Section::post_link`].
    pub region: &'a [u8],
}

/// The state of the post-link region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostLink<'a> {
    /// The section has no region (`region_len == 0`).
    None,
    /// Not written: the first four bytes are [`ERASED`].
    Erased,
    /// Written: a `u32` length `L`, then these `L` bytes.
    Written(&'a [u8]),
    /// Neither: a region of 1–3 bytes, or a length past the region's end.
    Malformed,
}

impl<'a> Section<'a> {
    /// The post-link region's state. AimDB defines only this envelope; what
    /// the written bytes mean is up to the tool that wrote them.
    pub fn post_link(&self) -> PostLink<'a> {
        if self.region.is_empty() {
            return PostLink::None;
        }
        let Some((len, rest)) = self.region.split_first_chunk::<4>() else {
            return PostLink::Malformed;
        };
        if *len == [ERASED; 4] {
            return PostLink::Erased;
        }
        // `get`, not `4 + len`: a length near u32::MAX would wrap on a 32-bit MCU.
        let len = usize::try_from(u32::from_le_bytes(*len)).ok();
        match len.and_then(|len| rest.get(..len)) {
            Some(data) => PostLink::Written(data),
            None => PostLink::Malformed,
        }
    }
}

/// Whether `payload` holds no CR or LF byte.
///
/// AimX replies carry the payload verbatim, and newline-framed transports
/// (UDS) end a frame at the first line break. Valid JSON has raw CR or LF only
/// as whitespace between tokens, so a compact serialization always passes.
pub fn payload_is_single_line(payload: &[u8]) -> bool {
    !payload.iter().any(|&b| b == b'\r' || b == b'\n')
}

/// Parse a section at the start of `bytes`, which may extend past its end (a
/// flash dump). Malformed input is an error, never a panic.
pub fn parse(bytes: &[u8]) -> Result<Section<'_>, ParseError> {
    if bytes.get(..MAGIC.len()) != Some(&MAGIC[..]) {
        return Err(ParseError::NoMagic);
    }
    let Some(header) = bytes.first_chunk::<HEADER_LEN>() else {
        return Err(ParseError::Truncated);
    };
    let u16_at = |at: usize| u16::from_le_bytes([header[at], header[at + 1]]);
    let u32_at = |at: usize| {
        u64::from(u32::from_le_bytes([
            header[at],
            header[at + 1],
            header[at + 2],
            header[at + 3],
        ]))
    };

    let format = u16_at(8);
    if format != FORMAT {
        return Err(ParseError::UnknownFormat(format));
    }
    // Widened to u64, so none of the sums below can overflow.
    let header_len = u64::from(u16_at(10));
    let payload_len = u32_at(12);
    let region_off = u32_at(16);
    let region_len = u32_at(20);
    let total_len = u32_at(24);

    if header_len < HEADER_LEN as u64
        || header_len + payload_len > region_off
        || region_off % 8 != 0
        || region_off + region_len != total_len
        || total_len > SECTION_MAX as u64
    {
        return Err(ParseError::Inconsistent);
    }
    if total_len > bytes.len() as u64 {
        return Err(ParseError::Truncated);
    }

    // Every bound is now at most `total_len`, which fits `bytes`.
    let payload = header_len as usize..(header_len + payload_len) as usize;
    let region = region_off as usize..total_len as usize;
    match (bytes.get(payload), bytes.get(region)) {
        (Some(payload), Some(region)) => Ok(Section {
            format,
            payload,
            region,
        }),
        _ => Err(ParseError::Truncated),
    }
}
