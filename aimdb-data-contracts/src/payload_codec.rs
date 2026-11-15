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
    /// Owned fallback for values larger than route scratch storage. Builtins
    /// override this with their native owned encoders. Custom encoders may do
    /// likewise; the default retries serialization into growing caller storage.
    fn encode_payload_owned<T: serde::Serialize>(
        &self,
        value: &T,
    ) -> Result<Vec<u8>, SerializeError> {
        let mut capacity = 256usize;
        loop {
            let mut out = Vec::new();
            out.try_reserve_exact(capacity)
                .map_err(|_| SerializeError::InvalidData)?;
            out.resize(capacity, 0);
            match self.encode_payload(value, &mut out) {
                Ok(n) if n <= out.len() => {
                    out.truncate(n);
                    return Ok(out);
                }
                Ok(_) => return Err(SerializeError::InvalidData),
                Err(SerializeError::BufferTooSmall) => {
                    capacity = capacity.checked_mul(2).ok_or(SerializeError::InvalidData)?
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Encodes into caller storage and returns the byte count.
    ///
    /// Returns `BufferTooSmall` if storage is insufficient. The destination may
    /// have been partially written on error. Built-in Postcard and CDR encoders
    /// do not allocate on success; JSON retains its existing owned encoding path.
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
    fn encode_payload_owned<T: serde::Serialize>(
        &self,
        value: &T,
    ) -> Result<Vec<u8>, SerializeError> {
        serde_json::to_vec(value).map_err(|_| SerializeError::InvalidData)
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
    fn encode_payload_owned<T: serde::Serialize>(
        &self,
        value: &T,
    ) -> Result<Vec<u8>, SerializeError> {
        postcard::to_allocvec(value).map_err(|_| SerializeError::InvalidData)
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
    fn encode_payload_owned<T: serde::Serialize>(
        &self,
        value: &T,
    ) -> Result<Vec<u8>, SerializeError> {
        aimdb_cdr::to_vec(value).map_err(|_| SerializeError::InvalidData)
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
