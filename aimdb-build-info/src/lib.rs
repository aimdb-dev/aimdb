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

/// Name of the section in ELF files.
pub const ELF_SECTION: &str = ".aimdb_build_info";

/// Segment and section name of the section in Mach-O files.
pub const MACHO_SECTION: (&str, &str) = ("__DATA", "__aimdb_bi");

/// The global symbol at the section's start, as Rust names it (Mach-O adds a
/// leading underscore).
pub const SYMBOL: &str = "__AIMDB_BUILD_INFO";

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

#[doc(hidden)]
pub mod __private {
    pub use core;

    /// FNV-1a of the section image. [`section!`](crate::section) writes it
    /// into the assembly text: `.incbin` reads the file only when assembling,
    /// so without it an incremental build that sees an image of the same
    /// length reuses the old object.
    pub const fn fnv1a(bytes: &[u8]) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut i = 0;
        while i < bytes.len() {
            hash ^= bytes[i] as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            i += 1;
        }
        hash
    }

    /// Whether `.incbin` can name a file under `dir`: its quoted string ends
    /// at `"` and treats `\` as an escape.
    pub const fn incbin_path_ok(dir: &str) -> bool {
        let bytes = dir.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'"' || bytes[i] == b'\\' {
                return false;
            }
            i += 1;
        }
        true
    }
}

/// Place this binary's build-info section and return its bytes.
///
/// The bytes come from `OUT_DIR/aimdb_build_info.bin`, which the build script
/// writes with `BuildInfo::write` (feature `build`). If the build fails with
/// that file missing, the build script does not call `write()`.
///
/// Call it once per binary. The section's symbol, `__AIMDB_BUILD_INFO`, is
/// global, so a second call is a duplicate-symbol link error, and tools can
/// find the section by that symbol as well as by section name.
///
/// On ELF and Mach-O targets the section is assembled from the file with
/// `.incbin` and declared to Rust as an `extern` static. Rust never sees its
/// contents, so it cannot fold them into constants: every read is a load, and
/// a post-link region written into the binary after linking is what the
/// program reads. The section is kept by the linker even when nothing reads
/// it: on ELF by the `R` (retain) flag, on Mach-O by `.no_dead_strip`.
///
/// | Target | Section |
/// |---|---|
/// | ELF (Linux, bare-metal `none`, …) | `.aimdb_build_info` |
/// | Mach-O (Apple) | `__DATA,__aimdb_bi` |
/// | PE (Windows, UEFI), wasm | none: an ordinary `static`, served over AimX only, without a post-link region |
///
/// On ELF and Mach-O, `OUT_DIR` must not contain `"` or `\`, which the
/// assembler's `.incbin` cannot name; the build then fails with that message.
#[macro_export]
macro_rules! section {
    () => {{
        // A module, because `global_asm!` is allowed in item position only.
        mod __aimdb_build_info {
            const IMAGE: &[u8] = $crate::__private::core::include_bytes!(
                $crate::__private::core::concat!(
                    $crate::__private::core::env!("OUT_DIR"),
                    "/aimdb_build_info.bin"
                )
            );
            pub const LEN: usize = IMAGE.len();

            #[cfg(not(any(
                target_os = "windows",
                target_os = "uefi",
                target_family = "wasm"
            )))]
            const _: () = $crate::__private::core::assert!(
                $crate::__private::incbin_path_ok($crate::__private::core::env!("OUT_DIR")),
                "aimdb-build-info: OUT_DIR contains `\"` or `\\`, which `.incbin` cannot name"
            );

            #[cfg(target_vendor = "apple")]
            $crate::__private::core::arch::global_asm!(
                ".pushsection __DATA,__aimdb_bi",
                ".globl ___AIMDB_BUILD_INFO",
                ".no_dead_strip ___AIMDB_BUILD_INFO",
                ".p2align 3",
                "___AIMDB_BUILD_INFO:",
                $crate::__private::core::concat!(
                    ".incbin \"",
                    $crate::__private::core::env!("OUT_DIR"),
                    "/aimdb_build_info.bin\""
                ),
                ".popsection",
                "/* aimdb-build-info image {hash} */",
                hash = const $crate::__private::fnv1a(IMAGE),
            );

            #[cfg(not(any(
                target_vendor = "apple",
                target_os = "windows",
                target_os = "uefi",
                target_family = "wasm"
            )))]
            $crate::__private::core::arch::global_asm!(
                ".pushsection .aimdb_build_info,\"aR\",%progbits",
                ".globl __AIMDB_BUILD_INFO",
                ".type __AIMDB_BUILD_INFO,%object",
                ".p2align 3",
                "__AIMDB_BUILD_INFO:",
                $crate::__private::core::concat!(
                    ".incbin \"",
                    $crate::__private::core::env!("OUT_DIR"),
                    "/aimdb_build_info.bin\""
                ),
                ".size __AIMDB_BUILD_INFO, . - __AIMDB_BUILD_INFO",
                ".popsection",
                "/* aimdb-build-info image {hash} */",
                hash = const $crate::__private::fnv1a(IMAGE),
            );

            #[cfg(not(any(
                target_os = "windows",
                target_os = "uefi",
                target_family = "wasm"
            )))]
            unsafe extern "C" {
                pub static __AIMDB_BUILD_INFO: [u8; LEN];
            }

            #[cfg(any(
                target_os = "windows",
                target_os = "uefi",
                target_family = "wasm"
            ))]
            pub static __AIMDB_BUILD_INFO: [u8; LEN] =
                *$crate::__private::core::include_bytes!($crate::__private::core::concat!(
                    $crate::__private::core::env!("OUT_DIR"),
                    "/aimdb_build_info.bin"
                ));
        }

        // SAFETY: on ELF and Mach-O the symbol is the section assembled
        // above, exactly `LEN` bytes, never written while the program runs.
        #[allow(unused_unsafe)]
        let section: &'static [u8] = unsafe {
            &*$crate::__private::core::ptr::addr_of!(__aimdb_build_info::__AIMDB_BUILD_INFO)
        };
        section
    }};
}
