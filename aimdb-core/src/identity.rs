//! Instance identity: which build this binary is, and which instance runs it.
//!
//! The build comes from the binary's build-info section, placed by
//! [`aimdb_build_info::section!`]. The instance's name and unique ID are given
//! at startup: a name on a host, the chip's ID on a board. All three are
//! optional and set on the [`AimDbBuilder`](crate::AimDbBuilder); `build()`
//! checks them and [`AimDb::identity`](crate::AimDb::identity) returns them.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use aimdb_build_info::Section;

/// Longest instance name, in bytes of UTF-8.
pub const NAME_MAX: usize = 128;

/// Longest instance UID, in bytes.
pub const UID_MAX: usize = 32;

/// The identity frozen at `build()`.
#[derive(Debug, Clone, Default)]
pub struct Identity {
    name: Option<String>,
    uid: Option<Vec<u8>>,
    build_info: Option<Section<'static>>,
}

impl Identity {
    /// The instance name given at startup.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The instance's unique ID given at startup, e.g. the chip's.
    pub fn uid(&self) -> Option<&[u8]> {
        self.uid.as_deref()
    }

    /// This binary's build-info section. `build()` checked it: the payload is
    /// at most [`PAYLOAD_MAX`](aimdb_build_info::PAYLOAD_MAX) bytes, holds no
    /// line break, and (with the `remote` feature) is a JSON object.
    pub fn build_info(&self) -> Option<&Section<'static>> {
        self.build_info.as_ref()
    }
}

/// What the builder collects, checked by `build()`.
#[derive(Default)]
pub(crate) struct IdentityConfig {
    pub(crate) name: Option<String>,
    pub(crate) uid: Option<Vec<u8>>,
    pub(crate) build_info: Option<&'static [u8]>,
}

impl IdentityConfig {
    /// The identity, and a message for each mistake.
    pub(crate) fn freeze(self) -> (Identity, Vec<String>) {
        let mut errors = Vec::new();
        if let Some(name) = self.name.as_ref().filter(|n| n.len() > NAME_MAX) {
            errors.push(format!(
                "instance name is {} bytes, at most {NAME_MAX}",
                name.len()
            ));
        }
        if let Some(uid) = self.uid.as_ref().filter(|u| u.len() > UID_MAX) {
            errors.push(format!(
                "instance UID is {} bytes, at most {UID_MAX}",
                uid.len()
            ));
        }
        let build_info = self
            .build_info
            .and_then(|bytes| check_build_info(bytes).map_err(|e| errors.push(e)).ok());
        let identity = Identity {
            name: self.name,
            uid: self.uid,
            build_info,
        };
        (identity, errors)
    }
}

/// The section, if AimX can serve it: replies carry the payload verbatim.
fn check_build_info(bytes: &'static [u8]) -> Result<Section<'static>, String> {
    let section = aimdb_build_info::parse(bytes).map_err(|e| format!("build info: {e}"))?;
    let payload = section.payload;
    if payload.len() > aimdb_build_info::PAYLOAD_MAX {
        return Err(format!(
            "build info: the payload is {} bytes, at most {}",
            payload.len(),
            aimdb_build_info::PAYLOAD_MAX
        ));
    }
    // Newline-framed transports (UDS) would end the reply at the break.
    if !aimdb_build_info::payload_is_single_line(payload) {
        return Err("build info: the payload contains a line break".into());
    }
    #[cfg(feature = "remote")]
    check_json_object(payload).map_err(|e| format!("build info: {e}"))?;
    Ok(section)
}

/// Whether `payload` is one JSON object, checked without building a tree.
#[cfg(feature = "remote")]
fn check_json_object(payload: &[u8]) -> Result<(), serde_json::Error> {
    use serde::de::{Deserializer, IgnoredAny, MapAccess, Visitor};

    struct Object;

    impl<'de> Visitor<'de> for Object {
        type Value = ();

        fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.write_str("a JSON object")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok(())
        }
    }

    let mut de = serde_json::Deserializer::from_slice(payload);
    (&mut de).deserialize_map(Object)?;
    de.end()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::boxed::Box;
    use alloc::vec;

    /// A format-1 section around `payload`, by hand and without the build
    /// helper's limits, leaked to `'static` as `section!()` is.
    fn section(payload: &[u8]) -> &'static [u8] {
        let region_off = (32 + payload.len()).div_ceil(8) * 8;
        let total = region_off + 16;
        let mut b = vec![0u8; total];
        b[..8].copy_from_slice(&aimdb_build_info::MAGIC);
        b[8..10].copy_from_slice(&aimdb_build_info::FORMAT.to_le_bytes());
        b[10..12].copy_from_slice(&32u16.to_le_bytes());
        b[12..16].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        b[16..20].copy_from_slice(&(region_off as u32).to_le_bytes());
        b[20..24].copy_from_slice(&16u32.to_le_bytes());
        b[24..28].copy_from_slice(&(total as u32).to_le_bytes());
        b[32..32 + payload.len()].copy_from_slice(payload);
        b[region_off..].fill(aimdb_build_info::ERASED);
        Box::leak(b.into_boxed_slice())
    }

    fn with_build_info(bytes: &'static [u8]) -> (Identity, Vec<String>) {
        IdentityConfig {
            build_info: Some(bytes),
            ..Default::default()
        }
        .freeze()
    }

    #[test]
    fn unset_is_empty() {
        let (id, errors) = IdentityConfig::default().freeze();
        assert!(errors.is_empty());
        assert_eq!(id.name(), None);
        assert_eq!(id.uid(), None);
        assert!(id.build_info().is_none());
    }

    #[test]
    fn set_values_are_kept() {
        let (id, errors) = IdentityConfig {
            name: Some("gateway-3".into()),
            uid: Some(vec![0x00, 0x35, 0xab]),
            build_info: Some(section(br#"{"schema":1,"name":"app"}"#)),
        }
        .freeze();
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(id.name(), Some("gateway-3"));
        assert_eq!(id.uid(), Some(&[0x00, 0x35, 0xab][..]));
        assert_eq!(
            id.build_info().unwrap().payload,
            br#"{"schema":1,"name":"app"}"#
        );
    }

    #[test]
    fn name_and_uid_limits_are_inclusive() {
        let (_, errors) = IdentityConfig {
            name: Some("n".repeat(NAME_MAX)),
            uid: Some(vec![0; UID_MAX]),
            build_info: None,
        }
        .freeze();
        assert!(errors.is_empty(), "{errors:?}");

        let (_, errors) = IdentityConfig {
            name: Some("n".repeat(NAME_MAX + 1)),
            uid: Some(vec![0; UID_MAX + 1]),
            build_info: None,
        }
        .freeze();
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors[0].contains("instance name is 129 bytes"));
        assert!(errors[1].contains("instance UID is 33 bytes"));
    }

    #[test]
    fn sections_that_cannot_be_served_are_rejected() {
        let mut no_magic = section(b"{}").to_vec();
        no_magic[0] = b'X';
        let mut truncated = section(b"{}").to_vec();
        truncated.pop();
        // The parser accepts up to 64 KiB; AimX serves at most PAYLOAD_MAX.
        let mut big = vec![b' '; aimdb_build_info::PAYLOAD_MAX - 1];
        big[0] = b'{';
        big.extend_from_slice(b"}");
        big.push(b' ');

        for (bytes, message) in [
            (
                &*Box::leak(no_magic.into_boxed_slice()),
                "no build-info magic",
            ),
            (&*Box::leak(truncated.into_boxed_slice()), "truncated"),
            (section(&big), "4097 bytes, at most 4096"),
            (section(b"{\n}"), "line break"),
            (section(b"{\r}"), "line break"),
        ] {
            let (id, errors) = with_build_info(bytes);
            assert!(id.build_info().is_none());
            assert_eq!(errors.len(), 1, "{errors:?}");
            assert!(errors[0].starts_with("build info: "), "{errors:?}");
            assert!(errors[0].contains(message), "{message}: {errors:?}");
        }
        // Exactly PAYLOAD_MAX is fine.
        big.pop();
        assert!(with_build_info(section(&big)).1.is_empty());
    }

    #[cfg(feature = "remote")]
    #[test]
    fn payload_must_be_one_json_object() {
        for payload in [
            &b"[1,2]"[..],
            b"\"text\"",
            b"",
            b"{",
            b"{\"a\":1} {}",
            b"{\"a\":}",
        ] {
            let (_, errors) = with_build_info(section(payload));
            assert_eq!(errors.len(), 1, "{:?}", core::str::from_utf8(payload));
            assert!(errors[0].starts_with("build info: "), "{errors:?}");
        }
        // Whitespace between tokens, escapes, nesting and unknown fields are
        // fine; only raw line breaks are not.
        let ok = br#"{ "schema": 1,	"ext": {"note": "a\nb", "list": [1, {"x": null}]} }"#;
        assert!(with_build_info(section(ok)).1.is_empty());
    }

    #[tokio::test]
    async fn build_reports_identity_mistakes_with_the_rest() {
        use crate::executor::test_support::NoopRuntimeOps;
        let Err(err) = crate::AimDbBuilder::new()
            .runtime(alloc::sync::Arc::new(NoopRuntimeOps))
            .instance_name("n".repeat(NAME_MAX + 1))
            .build_info(section(b"{\n}"))
            .build()
            .await
        else {
            panic!("build must fail");
        };
        let crate::DbError::InvalidConfiguration { errors } = &err else {
            panic!("expected InvalidConfiguration, got {err:?}");
        };
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors
            .iter()
            .all(|e| e.record_key.is_empty() && e.url.is_none()));
        let text = alloc::format!("{err}");
        assert!(text.contains("instance name is 129 bytes"), "{text}");
        assert!(
            text.contains("build info: the payload contains a line break"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn identity_on_the_built_db() {
        use crate::executor::test_support::NoopRuntimeOps;
        let (db, _runner) = crate::AimDbBuilder::new()
            .runtime(alloc::sync::Arc::new(NoopRuntimeOps))
            .instance_name("gateway-3")
            .instance_uid(&[0xde, 0xad])
            .build_info(section(b"{}"))
            .build()
            .await
            .unwrap();
        let id = db.identity();
        assert_eq!(id.name(), Some("gateway-3"));
        assert_eq!(id.uid(), Some(&[0xde, 0xad][..]));
        assert_eq!(id.build_info().unwrap().payload, b"{}");

        let (db, _runner) = crate::AimDbBuilder::new()
            .runtime(alloc::sync::Arc::new(NoopRuntimeOps))
            .build()
            .await
            .unwrap();
        assert!(db.identity().name().is_none() && db.identity().build_info().is_none());
    }
}
