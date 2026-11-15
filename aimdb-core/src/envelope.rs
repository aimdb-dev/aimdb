//! Codec-independent value identity and the format-1 binary envelope (design 062).
//! Parsing borrows its payload and writing uses caller storage; neither allocates.

/// Identity claimed by one value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ContractRef {
    name_id: u32,
    version: u32,
    shape: Option<u64>,
}
impl ContractRef {
    /// Constructs an identity; shape is the locked hash prefix, when available.
    pub const fn new(name_id: u32, version: u32, shape: Option<u64>) -> Self {
        Self {
            name_id,
            version,
            shape,
        }
    }
    /// FNV-1a 32-bit hash of the contract name.
    pub const fn name_id(self) -> u32 {
        self.name_id
    }
    /// Schema version carried by this value.
    pub const fn version(self) -> u32 {
        self.version
    }
    /// Locked SHA-256 prefix for this version.
    pub const fn shape(self) -> Option<u64> {
        self.shape
    }
}

/// Stable FNV-1a identifier of a contract name.
pub const fn name_id(name: &str) -> u32 {
    let bytes = name.as_bytes();
    let mut hash = 0x811c9dc5u32;
    let mut i = 0;
    while i < bytes.len() {
        hash = (hash ^ bytes[i] as u32).wrapping_mul(0x01000193);
        i += 1;
    }
    hash
}

/// Codec identifiers reserved by envelope format 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PayloadCodec {
    Unspecified,
    Json,
    Postcard,
    Cdr,
}

/// Header parsing or encoding failure, independent of the payload codec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvelopeError {
    NotEnvelope,
    UnsupportedFormat,
    Malformed,
    BufferTooSmall,
    MissingContract,
}
impl core::fmt::Display for EnvelopeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl core::error::Error for EnvelopeError {}

/// A logical envelope borrowing the payload from a frame or static binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Envelope<'a> {
    contract: ContractRef,
    codec: PayloadCodec,
    payload: &'a [u8],
}
impl<'a> Envelope<'a> {
    /// Parses a configured inline frame. Never use magic-byte sniffing to choose a carrier.
    /// Reserved stamp and extension fields are validated and skipped.
    pub fn parse(frame: &'a [u8]) -> Result<Self, EnvelopeError> {
        let mut r = Reader {
            bytes: frame,
            pos: 0,
        };
        let magic = r.byte()?;
        if magic & 0xf0 != 0xa0 {
            return Err(EnvelopeError::NotEnvelope);
        }
        if magic != 0xa1 {
            return Err(EnvelopeError::UnsupportedFormat);
        }
        let flags = r.byte()?;
        if flags & 8 != 0 {
            return Err(EnvelopeError::UnsupportedFormat);
        }
        let codec = match flags >> 4 {
            0 => PayloadCodec::Unspecified,
            1 => PayloadCodec::Json,
            2 => PayloadCodec::Postcard,
            3 => PayloadCodec::Cdr,
            _ => return Err(EnvelopeError::UnsupportedFormat),
        };
        let name = r.take(4)?;
        let name_id = u32::from_le_bytes([name[0], name[1], name[2], name[3]]);
        let version = r.varint(32)? as u32;
        let shape = if flags & 1 != 0 {
            let b = r.take(8)?;
            Some(u64::from_le_bytes([
                b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
            ]))
        } else {
            None
        };
        if flags & 2 != 0 {
            r.varint(64)?;
        }
        if flags & 4 != 0 {
            let len = r.varint(8)? as usize;
            let mut ext = Reader {
                bytes: r.take(len)?,
                pos: 0,
            };
            while ext.pos < ext.bytes.len() {
                if ext.varint(16)? == 0 {
                    return Err(EnvelopeError::Malformed);
                }
                let len = ext.varint(8)? as usize;
                ext.take(len)?;
            }
        }
        Ok(Self::bound(
            ContractRef::new(name_id, version, shape),
            codec,
            &frame[r.pos..],
        ))
    }
    /// Builds the envelope implied by a bare link's static binding.
    pub const fn bound(contract: ContractRef, codec: PayloadCodec, payload: &'a [u8]) -> Self {
        Self {
            contract,
            codec,
            payload,
        }
    }
    pub const fn contract(&self) -> ContractRef {
        self.contract
    }
    pub const fn codec(&self) -> PayloadCodec {
        self.codec
    }
    pub const fn payload(&self) -> &'a [u8] {
        self.payload
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], EnvelopeError> {
        let end = self.pos.checked_add(n).ok_or(EnvelopeError::Malformed)?;
        let bytes = self
            .bytes
            .get(self.pos..end)
            .ok_or(EnvelopeError::Malformed)?;
        self.pos = end;
        Ok(bytes)
    }
    fn byte(&mut self) -> Result<u8, EnvelopeError> {
        Ok(self.take(1)?[0])
    }
    fn varint(&mut self, bits: u32) -> Result<u64, EnvelopeError> {
        let mut value = 0;
        let mut shift = 0;
        loop {
            let b = self.byte()?;
            let remaining = bits - shift;
            if remaining < 7 && (b & 0x7f) as u64 >= (1u64 << remaining) {
                return Err(EnvelopeError::Malformed);
            }
            value |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(value);
            }
            shift += 7;
            if shift >= bits {
                return Err(EnvelopeError::Malformed);
            }
        }
    }
}

/// Encodes at most 19 header bytes into caller storage, leaving room for the payload.
/// Format 1 writers currently emit neither stamps nor extensions.
pub struct HeaderWriter<'b> {
    out: &'b mut [u8],
    contract: Option<ContractRef>,
    codec: PayloadCodec,
}
impl<'b> HeaderWriter<'b> {
    pub fn new(out: &'b mut [u8]) -> Self {
        Self {
            out,
            contract: None,
            codec: PayloadCodec::Unspecified,
        }
    }
    pub fn contract(mut self, contract: ContractRef) -> Self {
        self.contract = Some(contract);
        self
    }
    pub fn codec(mut self, codec: PayloadCodec) -> Self {
        self.codec = codec;
        self
    }
    /// Returns the payload offset. On error the destination remains unchanged.
    pub fn finish(self) -> Result<usize, EnvelopeError> {
        let c = self.contract.ok_or(EnvelopeError::MissingContract)?;
        // Stage locally so an undersized destination is left untouched.
        let mut header = [0u8; 19];
        header[0] = 0xa1;
        header[1] = match self.codec {
            PayloadCodec::Unspecified => 0,
            PayloadCodec::Json => 0x10,
            PayloadCodec::Postcard => 0x20,
            PayloadCodec::Cdr => 0x30,
        } | u8::from(c.shape.is_some());
        header[2..6].copy_from_slice(&c.name_id.to_le_bytes());
        let mut version = c.version;
        let mut len = 6;
        loop {
            let b = (version & 0x7f) as u8;
            version >>= 7;
            header[len] = b | if version != 0 { 0x80 } else { 0 };
            len += 1;
            if version == 0 {
                break;
            }
        }
        if let Some(shape) = c.shape {
            header[len..len + 8].copy_from_slice(&shape.to_le_bytes());
            len += 8;
        }
        self.out
            .get_mut(..len)
            .ok_or(EnvelopeError::BufferTooSmall)?
            .copy_from_slice(&header[..len]);
        Ok(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_boundaries() {
        for version in [0, 1, 127, 128, 16383, 16384, u32::MAX] {
            for shape in [None, Some(0), Some(u64::MAX), Some(0x0102030405060708)] {
                for codec in [
                    PayloadCodec::Unspecified,
                    PayloadCodec::Json,
                    PayloadCodec::Postcard,
                    PayloadCodec::Cdr,
                ] {
                    let c = ContractRef::new(0x12345678, version, shape);
                    let mut bytes = [0; 24];
                    let n = HeaderWriter::new(&mut bytes)
                        .contract(c)
                        .codec(codec)
                        .finish()
                        .unwrap();
                    assert!(n <= 19);
                    bytes[n..n + 3].copy_from_slice(b"abc");
                    assert_eq!(
                        Envelope::parse(&bytes[..n + 3]),
                        Ok(Envelope::bound(c, codec, b"abc"))
                    );
                    for end in 0..n {
                        assert!(Envelope::parse(&bytes[..end]).is_err());
                    }
                    let mut short = [0x55; 19];
                    assert_eq!(
                        HeaderWriter::new(&mut short[..n - 1]).contract(c).finish(),
                        Err(EnvelopeError::BufferTooSmall)
                    );
                    assert_eq!(short, [0x55; 19]);
                }
            }
        }
    }
    #[test]
    fn golden_and_forward_fields() {
        let mut bytes = [0; 19];
        let n = HeaderWriter::new(&mut bytes)
            .contract(ContractRef::new(0x12345678, 128, None))
            .finish()
            .unwrap();
        assert_eq!(&bytes[..n], &[0xa1, 0, 0x78, 0x56, 0x34, 0x12, 0x80, 1]);
        let frame = [0xa1, 6, 1, 0, 0, 0, 1, 0xff, 1, 4, 64, 2, 9, 9, 42];
        assert_eq!(Envelope::parse(&frame).unwrap().payload(), &[42]);
        assert_eq!(
            Envelope::parse(&[0xa2]),
            Err(EnvelopeError::UnsupportedFormat)
        );
        assert_eq!(
            Envelope::parse(&[0xa1, 8]),
            Err(EnvelopeError::UnsupportedFormat)
        );
        assert_eq!(
            Envelope::parse(&[0xa1, 0x40]),
            Err(EnvelopeError::UnsupportedFormat)
        );
        assert_eq!(Envelope::parse(b"{}"), Err(EnvelopeError::NotEnvelope));
        assert!(Envelope::parse(&[0xa1, 0, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 0x10]).is_err());
        assert_eq!(
            Envelope::parse(&[0xa1, 0, 0, 0, 0, 0, 0x81, 0])
                .unwrap()
                .contract()
                .version(),
            1
        );
    }
    #[test]
    fn malformed_optional_fields() {
        for tail in [
            &[0x80][..], // truncated stamp
            &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 2],
        ] {
            let mut frame = vec![0xa1, 2, 0, 0, 0, 0, 1];
            frame.extend_from_slice(tail);
            assert_eq!(Envelope::parse(&frame), Err(EnvelopeError::Malformed));
        }
        for tail in [
            &[0x80, 2][..],         // extension section exceeds 255
            &[1, 1],                // missing TLV length
            &[2, 0, 0],             // reserved tag zero
            &[3, 1, 2, 9],          // TLV runs past section
            &[4, 0xff, 0xff, 4, 0], // overflowing u16 tag
        ] {
            let mut frame = vec![0xa1, 4, 0, 0, 0, 0, 1];
            frame.extend_from_slice(tail);
            assert_eq!(Envelope::parse(&frame), Err(EnvelopeError::Malformed));
        }
        let mut frame = vec![0xa1, 4, 0, 0, 0, 0, 1, 0xff, 1, 64, 0xfc, 1];
        frame.extend_from_slice(&[9; 252]);
        frame.push(42);
        assert_eq!(Envelope::parse(&frame).unwrap().payload(), &[42]);
        assert_eq!(
            HeaderWriter::new(&mut []).finish(),
            Err(EnvelopeError::MissingContract)
        );
    }

    #[test]
    fn arbitrary_frames_do_not_panic() {
        let mut state = 42u64;
        let mut bytes = [0; 64];
        for i in 0..100_000 {
            for byte in &mut bytes {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *byte = state as u8;
            }
            if i % 2 == 0 {
                bytes[0] = 0xa1;
            }
            let _ = Envelope::parse(&bytes[..i % 65]);
        }
    }
}
