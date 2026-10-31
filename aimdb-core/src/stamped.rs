//! The record-value envelope: a value with the metadata of the write that
//! stored it (design 058 §4.1).
//!
//! `value` is public; metadata is private behind accessors, so neither
//! [`Stamped`] nor [`WriteMetadata`] locks callers into a field layout. A later
//! metadata field adds an accessor and changes no existing call.

use crate::clock::Stamp;

/// A record value together with the metadata of the write that stored it.
#[derive(Clone, Debug, PartialEq)]
pub struct Stamped<T> {
    pub value: T,
    metadata: WriteMetadata,
}

/// Metadata of one record write. Fields are private; read them through
/// accessors.
///
/// `Copy` is a commitment: write metadata stays small, fixed-size and
/// allocation-free, since every buffer slot carries one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteMetadata {
    stamp: Stamp,
}

impl WriteMetadata {
    /// Created by `RecordWriter` at write time.
    #[allow(dead_code)] // the write path stamps from checkpoint B on
    pub(crate) fn new(stamp: Stamp) -> Self {
        Self { stamp }
    }

    /// Metadata decoded from a peer's wire value, or built in tests.
    /// Carries exactly what the wire carries; never reaches a buffer.
    pub fn from_wire_stamp(stamp: Stamp) -> Self {
        Self { stamp }
    }

    pub fn stamp(&self) -> Stamp {
        self.stamp
    }
}

impl<T> Stamped<T> {
    pub fn new(value: T, metadata: WriteMetadata) -> Self {
        Self { value, metadata }
    }

    pub fn metadata(&self) -> &WriteMetadata {
        &self.metadata
    }

    pub fn stamp(&self) -> Stamp {
        self.metadata.stamp()
    }

    pub fn into_value(self) -> T {
        self.value
    }

    // Same write, different representation: metadata is kept as a whole.

    pub fn as_ref(&self) -> Stamped<&T> {
        Stamped {
            value: &self.value,
            metadata: self.metadata,
        }
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Stamped<U> {
        Stamped {
            value: f(self.value),
            metadata: self.metadata,
        }
    }

    pub fn try_map<U, E>(self, f: impl FnOnce(T) -> Result<U, E>) -> Result<Stamped<U>, E> {
        Ok(Stamped {
            value: f(self.value)?,
            metadata: self.metadata,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Stamped<u32> {
        Stamped::new(7, WriteMetadata::new(Stamp::from_nanos(42)))
    }

    #[test]
    fn conversions_keep_metadata() {
        let s = sample();
        let meta = *s.metadata();
        assert_eq!(*s.as_ref().metadata(), meta);
        assert_eq!(*s.as_ref().value, 7);
        let mapped = s.clone().map(|v| v * 2);
        assert_eq!((mapped.value, *mapped.metadata()), (14, meta));
        let tried = s.clone().try_map(|v| Ok::<_, ()>(v + 1)).unwrap();
        assert_eq!((tried.value, *tried.metadata()), (8, meta));
        assert_eq!(s.stamp(), Stamp::from_nanos(42));
        assert_eq!(s.into_value(), 7);
    }

    #[test]
    fn try_map_error_passes_through() {
        let err = sample().try_map(|_| Err::<u32, _>("boom")).unwrap_err();
        assert_eq!(err, "boom");
    }

    #[test]
    fn wire_metadata_carries_the_stamp() {
        let meta = WriteMetadata::from_wire_stamp(Stamp::from_nanos(9));
        assert_eq!(meta.stamp().as_nanos(), 9);
    }
}
