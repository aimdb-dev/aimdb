//! Generic codec operations used to decode historical contract types.
//!
//! Unlike `LinkCodec<T>`, these traits operate on any serde type. A migration
//! chain chooses the historical type before invoking the codec, so it needs no
//! JSON version probe. They are deliberately not blanket-implemented for
//! `LinkCodec<T>`: a codec for a single concrete type cannot decode an arbitrary
//! historical type. Custom codecs implement these traits explicitly.

use aimdb_core::connector::SerializeError;
#[cfg(any(
    feature = "linkable-json",
    feature = "linkable-postcard",
    feature = "linkable-cdr"
))]
use alloc::string::ToString;
use alloc::{string::String, vec::Vec};

/// The single inbound rule: one transport frame carries exactly one value.
///
/// `used` is the decoder's consumed count, `len` the payload length. Bytes the
/// decoder did not consume (beyond what its format declares, such as CDR
/// padding or JSON whitespace) are an error. Every inbound codec path, plain
/// `with_link_codec` and contract admission, applies this one check.
pub(crate) fn check_consumed(used: usize, len: usize) -> Result<(), String> {
    if used == len {
        Ok(())
    } else {
        Err(alloc::format!(
            "decoder consumed {used} of {len} payload bytes"
        ))
    }
}

/// Generic decoding half of a link codec.
pub trait PayloadDecoder {
    /// Decodes one value and reports how many input bytes it consumed.
    ///
    /// The count must be at most `bytes.len()`. Admission uses it to reject
    /// trailing bytes on statically bound links. JSON whitespace following the
    /// value counts as consumed; transport framing never belongs to this count.
    fn decode_payload<T: serde::de::DeserializeOwned>(
        &self,
        bytes: &[u8],
    ) -> Result<(T, usize), String>;
}

/// Generic encoding half of a link codec.
pub trait PayloadEncoder {
    /// Owned fallback for values larger than route scratch storage: appends
    /// the encoding after the existing contents of `out`, so a caller can put
    /// a header first without moving the payload afterwards.
    ///
    /// Builtins override this with their native appending encoders. The
    /// default encodes into the growing tail of `out` with
    /// [`encode_payload`](Self::encode_payload), doubling the tail each time
    /// that returns `BufferTooSmall`. On error `out` may hold
    /// partial output after its original contents.
    fn encode_payload_append<T: serde::Serialize>(
        &self,
        value: &T,
        out: &mut Vec<u8>,
    ) -> Result<(), SerializeError> {
        let start = out.len();
        let mut capacity = 256usize;
        loop {
            let end = start
                .checked_add(capacity)
                .ok_or(SerializeError::InvalidData)?;
            out.try_reserve_exact(end.saturating_sub(out.len()))
                .map_err(|_| SerializeError::InvalidData)?;
            out.resize(end, 0);
            match self.encode_payload(value, &mut out[start..]) {
                Ok(n) if n <= capacity => {
                    out.truncate(start + n);
                    return Ok(());
                }
                Ok(_) => return Err(SerializeError::InvalidData),
                Err(SerializeError::BufferTooSmall) => {
                    capacity = capacity.checked_mul(2).ok_or(SerializeError::InvalidData)?
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Encodes into a new `Vec`, via
    /// [`encode_payload_append`](Self::encode_payload_append).
    fn encode_payload_owned<T: serde::Serialize>(
        &self,
        value: &T,
    ) -> Result<Vec<u8>, SerializeError> {
        let mut out = Vec::new();
        self.encode_payload_append(value, &mut out)?;
        Ok(out)
    }

    /// Encodes into caller storage and returns the byte count.
    ///
    /// Returns `BufferTooSmall` if storage is insufficient, and only then: it
    /// promises that a larger `out` would succeed. The default
    /// [`encode_payload_append`](Self::encode_payload_append) retries with
    /// doubled capacity, without an upper bound, for as long as it gets this
    /// error. Report any other limit, such as a maximum encoded size, as
    /// `InvalidData`. The destination may have been partially written on error.
    ///
    /// Built-in Postcard and CDR encoders do not allocate on success; JSON
    /// retains its existing owned encoding path.
    fn encode_payload<T: serde::Serialize>(
        &self,
        value: &T,
        out: &mut [u8],
    ) -> Result<usize, SerializeError>;
}

#[cfg(feature = "linkable-json")]
impl PayloadDecoder for crate::link_codecs::Json {
    fn decode_payload<T: serde::de::DeserializeOwned>(
        &self,
        bytes: &[u8],
    ) -> Result<(T, usize), String> {
        let mut stream = serde_json::Deserializer::from_slice(bytes).into_iter::<T>();
        let value = stream
            .next()
            .ok_or_else(|| String::from("expected JSON value"))?
            .map_err(|error| error.to_string())?;
        let mut used = stream.byte_offset();
        while matches!(bytes.get(used), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            used += 1;
        }
        Ok((value, used))
    }
}

#[cfg(feature = "linkable-json")]
impl PayloadEncoder for crate::link_codecs::Json {
    fn encode_payload_append<T: serde::Serialize>(
        &self,
        value: &T,
        out: &mut Vec<u8>,
    ) -> Result<(), SerializeError> {
        // `io::Write` for `Vec` needs std; no_std + alloc copies once.
        #[cfg(feature = "std")]
        return serde_json::to_writer(out, value).map_err(|_| SerializeError::InvalidData);
        #[cfg(not(feature = "std"))]
        {
            let encoded = serde_json::to_vec(value).map_err(|_| SerializeError::InvalidData)?;
            out.extend_from_slice(&encoded);
            Ok(())
        }
    }

    fn encode_payload<T: serde::Serialize>(
        &self,
        value: &T,
        out: &mut [u8],
    ) -> Result<usize, SerializeError> {
        let encoded = serde_json::to_vec(value).map_err(|_| SerializeError::InvalidData)?;
        out.get_mut(..encoded.len())
            .ok_or(SerializeError::BufferTooSmall)?
            .copy_from_slice(&encoded);
        Ok(encoded.len())
    }
}

#[cfg(feature = "linkable-postcard")]
impl<const N: usize> PayloadDecoder for crate::link_codecs::Postcard<N> {
    fn decode_payload<T: serde::de::DeserializeOwned>(
        &self,
        bytes: &[u8],
    ) -> Result<(T, usize), String> {
        let (value, remainder) =
            postcard::take_from_bytes(bytes).map_err(|error| error.to_string())?;
        Ok((value, bytes.len() - remainder.len()))
    }
}

#[cfg(feature = "linkable-postcard")]
impl<const N: usize> PayloadEncoder for crate::link_codecs::Postcard<N> {
    fn encode_payload_append<T: serde::Serialize>(
        &self,
        value: &T,
        out: &mut Vec<u8>,
    ) -> Result<(), SerializeError> {
        // `to_extend` takes its writer by value; borrow `out` through it.
        struct Append<'a>(&'a mut Vec<u8>);
        impl Extend<u8> for Append<'_> {
            fn extend<I: IntoIterator<Item = u8>>(&mut self, bytes: I) {
                self.0.extend(bytes);
            }
        }
        postcard::to_extend(value, Append(out))
            .map(|_| ())
            .map_err(|_| SerializeError::InvalidData)
    }

    fn encode_payload<T: serde::Serialize>(
        &self,
        value: &T,
        out: &mut [u8],
    ) -> Result<usize, SerializeError> {
        match postcard::to_slice(value, out) {
            Ok(used) => Ok(used.len()),
            Err(postcard::Error::SerializeBufferFull) => Err(SerializeError::BufferTooSmall),
            Err(_) => Err(SerializeError::InvalidData),
        }
    }
}

#[cfg(feature = "linkable-cdr")]
impl<const N: usize> PayloadDecoder for crate::link_codecs::Cdr<N> {
    fn decode_payload<T: serde::de::DeserializeOwned>(
        &self,
        bytes: &[u8],
    ) -> Result<(T, usize), String> {
        aimdb_cdr::from_bytes_with_consumed(bytes).map_err(|error| error.to_string())
    }
}

#[cfg(feature = "linkable-cdr")]
impl<const N: usize> PayloadEncoder for crate::link_codecs::Cdr<N> {
    fn encode_payload_append<T: serde::Serialize>(
        &self,
        value: &T,
        out: &mut Vec<u8>,
    ) -> Result<(), SerializeError> {
        aimdb_cdr::append_to_vec(value, out).map_err(|_| SerializeError::InvalidData)
    }

    fn encode_payload<T: serde::Serialize>(
        &self,
        value: &T,
        out: &mut [u8],
    ) -> Result<usize, SerializeError> {
        match aimdb_cdr::to_slice(value, out) {
            Ok(used) => Ok(used),
            Err(aimdb_cdr::Error::BufferTooSmall) => Err(SerializeError::BufferTooSmall),
            Err(_) => Err(SerializeError::InvalidData),
        }
    }
}
