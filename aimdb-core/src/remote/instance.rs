//! `instance.info` and `instance.post_link` (protocol 3.1): which build and
//! which instance this is. Every AimX transport, the WebSocket one included,
//! answers with these replies, so the answer is the same on each.

use alloc::string::String;
use alloc::vec::Vec;

use aimdb_build_info::PostLink;
use serde::Serialize;
use serde_json::value::RawValue;
use serde_json::Value;

use super::protocol::{InstanceIdentityReply, PostLinkChunk, PostLinkReply, POST_LINK_CHUNK_MAX};
use crate::AimDb;

/// Why `instance.post_link` has no chunk to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostLinkError {
    /// No build info is embedded, or its post-link region is not written.
    /// Dispatches answer `not_found`.
    NotWritten,
    /// `params` is not an object of optional `offset` and `len`, each an
    /// integer in `0..=u32::MAX`. Dispatches answer `internal`, as for other
    /// methods' malformed params.
    InvalidParams,
}

/// The `instance.info` reply for `db`, as JSON bytes.
///
/// The build-info payload is spliced in as embedded: `build()` checked that
/// it is one JSON object without line breaks, and it is never decoded into a
/// tree. The reply is at most
/// [`INSTANCE_INFO_REPLY_MAX`](super::INSTANCE_INFO_REPLY_MAX) bytes.
pub fn instance_info_reply(db: &AimDb) -> Vec<u8> {
    #[derive(Serialize)]
    struct Reply<'a> {
        aimdb_version: &'static str,
        instance: InstanceIdentityReply,
        build: Option<Build<'a>>,
    }

    #[derive(Serialize)]
    struct Build<'a> {
        format: u16,
        info: &'a RawValue,
        post_link: Option<PostLinkReply>,
    }

    let identity = db.identity();
    let build = identity.build_info().and_then(|section| {
        Some(Build {
            format: section.format,
            // Validates without building a tree; cannot fail after build().
            info: serde_json::from_slice(section.payload).ok()?,
            post_link: match section.post_link() {
                PostLink::None => None,
                PostLink::Erased => Some(PostLinkReply::Erased),
                PostLink::Written(data) => Some(PostLinkReply::Written {
                    len: data.len() as u32,
                }),
                PostLink::Malformed => Some(PostLinkReply::Malformed),
            },
        })
    });
    let reply = Reply {
        aimdb_version: env!("CARGO_PKG_VERSION"),
        instance: InstanceIdentityReply {
            name: identity.name().map(String::from),
            uid: identity.uid().map(hex),
        },
        build,
    };
    serde_json::to_vec(&reply).unwrap_or_default()
}

/// The `instance.post_link` reply for `db` and the request's `params`, as
/// JSON bytes: one chunk of the written post-link bytes.
///
/// `params` is `{"offset": u32, "len": u32}`, both optional; `null` counts as
/// `{}`. A missing `offset` is 0, a missing `len` or one over
/// [`POST_LINK_CHUNK_MAX`] is that maximum, and `len: 0` or an `offset` at or
/// past the end gives empty `data` with `total` set. The params are checked
/// before the region, so malformed ones are reported even without a region.
pub fn post_link_chunk_reply(db: &AimDb, params: &Value) -> Result<Vec<u8>, PostLinkError> {
    let (offset, len) = post_link_params(params)?;
    let data = match db.identity().build_info().map(|s| s.post_link()) {
        Some(PostLink::Written(data)) => data,
        _ => return Err(PostLinkError::NotWritten),
    };
    let len = len.unwrap_or(POST_LINK_CHUNK_MAX).min(POST_LINK_CHUNK_MAX);
    let start = (offset as usize).min(data.len());
    let end = start.saturating_add(len as usize).min(data.len());
    let chunk = PostLinkChunk {
        offset,
        // A section is at most 64 KiB.
        total: data.len() as u32,
        data: hex(&data[start..end]),
    };
    Ok(serde_json::to_vec(&chunk).unwrap_or_default())
}

/// `offset` (default 0) and `len` from the params; no clamping or defaulting
/// of malformed values.
fn post_link_params(params: &Value) -> Result<(u32, Option<u32>), PostLinkError> {
    let field = |key: &str| match params.get(key) {
        None => Ok(None),
        Some(v) => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .map(Some)
            .ok_or(PostLinkError::InvalidParams),
    };
    match params {
        Value::Null => Ok((0, None)),
        Value::Object(_) => Ok((field("offset")?.unwrap_or(0), field("len")?)),
        _ => Err(PostLinkError::InvalidParams),
    }
}

/// Lowercase hex.
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[usize::from(b >> 4)] as char);
        s.push(DIGITS[usize::from(b & 0x0f)] as char);
    }
    s
}
