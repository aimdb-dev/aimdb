//! The attachment every rmw_zenoh publication carries.

/// Its length: sequence number, source timestamp, the `0x10` length of the
/// GID array, and the GID.
pub(crate) const ATTACHMENT_LEN: usize = 8 + 8 + 1 + 16;

/// A publication's attachment, as zenoh-cpp's `ext::Serializer` writes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Attachment {
    /// Per publisher, starting at 1.
    pub sequence: i64,
    /// Nanoseconds since the Unix epoch; 0 without a wall clock.
    pub timestamp_ns: i64,
    pub gid: [u8; 16],
}

impl Attachment {
    pub(crate) fn encode(&self) -> [u8; ATTACHMENT_LEN] {
        let mut out = [0; ATTACHMENT_LEN];
        out[..8].copy_from_slice(&self.sequence.to_le_bytes());
        out[8..16].copy_from_slice(&self.timestamp_ns.to_le_bytes());
        out[16] = 16;
        out[17..].copy_from_slice(&self.gid);
        out
    }

    /// `None` unless `bytes` has exactly this layout.
    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes: &[u8; ATTACHMENT_LEN] = bytes.try_into().ok()?;
        if bytes[16] != 16 {
            return None;
        }
        Some(Self {
            sequence: i64::from_le_bytes(bytes[..8].try_into().ok()?),
            timestamp_ns: i64::from_le_bytes(bytes[8..16].try_into().ok()?),
            gid: bytes[17..].try_into().ok()?,
        })
    }
}
