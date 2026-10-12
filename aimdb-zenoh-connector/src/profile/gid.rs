//! Entity GIDs.

/// The GID rmw_zenoh derives from an entity's liveliness token: XXH3-128 of
/// the token, the low 64 bits then the high 64 bits, each little-endian.
pub(crate) fn gid(token: &str) -> [u8; 16] {
    let hash = twox_hash::XxHash3_128::oneshot(token.as_bytes());
    let mut out = [0; 16];
    out[..8].copy_from_slice(&(hash as u64).to_le_bytes());
    out[8..].copy_from_slice(&((hash >> 64) as u64).to_le_bytes());
    out
}
