//! Allocation-free identity lookup in the canonical, generated `aimdb.lock`.

/// The locked shape of a contract version, absent when it has not been locked.
///
/// `catalog!` implements this trait for its contracts. A contract outside a
/// catalog can opt into versioned admission with `impl ContractShape for T {}`.
/// There is no blanket impl, so catalog implementations remain coherent.
pub trait ContractShape {
    const SHAPE: Option<u64> = None;
}

/// FNV-1a 32 of the UTF-8 contract name, identical to core's envelope name ID.
///
/// A copy of `aimdb_core::envelope::name_id` on purpose: `aimdb-core` is an
/// optional dependency here, and `catalog!` needs the ID without it. A test
/// holds the two equal.
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

/// Resolves a shape prefix from the lock writer's canonical format.
///
/// This is deliberately not a general TOML parser. The dev-only lock test
/// validates the whole file, including its canonical rendering. Runtime builds
/// use only this const byte scan, with no TOML or SHA-256 dependency.
#[doc(hidden)]
pub const fn locked_shape(lock: &str, name: &str, version: u32) -> Option<u64> {
    let bytes = lock.as_bytes();
    let mut pos = 0;
    let mut supported = false;
    while pos < bytes.len() {
        let (start, end, next) = line(bytes, pos);
        pos = next;
        if equal(bytes, start, end, b"version = 1") {
            supported = true;
        }
        if equal(bytes, start, end, b"[[contract]]") {
            if !supported {
                return None;
            }
            let (ns, ne, next) = line(bytes, pos);
            let (vs, ve, next) = line(bytes, next);
            let (ss, se, next) = line(bytes, next);
            pos = next;
            if quoted_name_matches(bytes, ns, ne, name.as_bytes()) {
                if let Some(v) = decimal(bytes, vs, ve) {
                    if v == version {
                        return shape(bytes, ss, se);
                    }
                }
            }
        }
    }
    None
}

const fn line(bytes: &[u8], start: usize) -> (usize, usize, usize) {
    let mut end = start;
    while end < bytes.len() && bytes[end] != b'\n' {
        end += 1;
    }
    let next = if end < bytes.len() { end + 1 } else { end };
    let end = if end > start && bytes[end - 1] == b'\r' {
        end - 1
    } else {
        end
    };
    (start, end, next)
}
const fn equal(bytes: &[u8], start: usize, end: usize, expected: &[u8]) -> bool {
    if end - start != expected.len() {
        return false;
    }
    let mut i = 0;
    while i < expected.len() {
        if bytes[start + i] != expected[i] {
            return false;
        }
        i += 1;
    }
    true
}
const fn prefix(bytes: &[u8], start: usize, end: usize, expected: &[u8]) -> bool {
    end - start >= expected.len() && equal(bytes, start, start + expected.len(), expected)
}
const fn quoted_name_matches(bytes: &[u8], start: usize, end: usize, name: &[u8]) -> bool {
    if !prefix(bytes, start, end, b"name = \"") {
        return false;
    }
    let mut pos = start + 8;
    let mut i = 0;
    while i < name.len() {
        let byte = name[i];
        // Match precisely the escaping performed by lock::quote, including C1 controls.
        let control = byte < 32
            || byte == 127
            || (byte == 0xc2 && i + 1 < name.len() && name[i + 1] >= 0x80 && name[i + 1] <= 0x9f);
        if control {
            let code = if byte == 0xc2 {
                i += 1;
                name[i]
            } else {
                byte
            };
            let hex = b"0123456789ABCDEF";
            let expected = [
                b'\\',
                b'u',
                b'0',
                b'0',
                hex[(code >> 4) as usize],
                hex[(code & 15) as usize],
            ];
            if !prefix(bytes, pos, end, &expected) {
                return false;
            }
            pos += 6;
        } else {
            if byte == b'"' || byte == b'\\' {
                if pos >= end || bytes[pos] != b'\\' {
                    return false;
                }
                pos += 1;
            }
            if pos >= end || bytes[pos] != byte {
                return false;
            }
            pos += 1;
        }
        i += 1;
    }
    pos + 1 == end && bytes[pos] == b'"'
}
const fn decimal(bytes: &[u8], start: usize, end: usize) -> Option<u32> {
    if !prefix(bytes, start, end, b"version = ") {
        return None;
    }
    let mut pos = start + 10;
    if pos == end {
        return None;
    }
    let mut value = 0u32;
    while pos < end {
        let b = bytes[pos];
        if b < b'0' || b > b'9' {
            return None;
        }
        value = match value.checked_mul(10) {
            Some(v) => match v.checked_add((b - b'0') as u32) {
                Some(v) => v,
                None => return None,
            },
            None => return None,
        };
        pos += 1;
    }
    Some(value)
}
const fn hex(b: u8) -> Option<u64> {
    match b {
        b'0'..=b'9' => Some((b - b'0') as u64),
        b'a'..=b'f' => Some((b - b'a' + 10) as u64),
        _ => None,
    }
}
const fn shape(bytes: &[u8], start: usize, end: usize) -> Option<u64> {
    if !prefix(bytes, start, end, b"shape = \"sha256:")
        || end - start != 81
        || bytes[end - 1] != b'"'
    {
        return None;
    }
    let mut value = 0;
    let mut i = 0;
    while i < 64 {
        let digit = match hex(bytes[start + 16 + i]) {
            Some(v) => v,
            None => return None,
        };
        if i < 16 {
            value = (value << 4) | digit;
        }
        i += 1;
    }
    Some(value)
}

/// Whether two distinct contract names in `catalog` share an FNV-1a name_id.
///
/// Returns instead of panicking: `catalog!` asserts on the result in its own
/// const item, so the diagnostic has no frame inside this function (and none
/// quoting core's panic source, which differs with and without `rust-src`).
#[doc(hidden)]
pub const fn has_name_id_collision(catalog: &[crate::ContractId]) -> bool {
    let mut i = 0;
    while i < catalog.len() {
        let mut j = 0;
        while j < i {
            if catalog[i].name_id() == catalog[j].name_id()
                && !equal(
                    catalog[i].name.as_bytes(),
                    0,
                    catalog[i].name.len(),
                    catalog[j].name.as_bytes(),
                )
            {
                return true;
            }
            j += 1;
        }
        i += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    const LOCK: &str = concat!(
        "version = 1\n\n[[contract]]\nname = \"temperature\"\nversion = 1\n",
        "shape = \"sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"\n",
        "\n[[contract]]\nname = \"temperature\"\nversion = 4294967295\n",
        "shape = \"sha256:fedcba98765432100123456789abcdef0123456789abcdef0123456789abcdef\"\n",
    );
    const SHAPE: Option<u64> = locked_shape(LOCK, "temperature", 1);

    #[test]
    fn locked_versions_and_missing_entries() {
        assert_eq!(SHAPE, Some(0x0123456789abcdef));
        assert_eq!(
            locked_shape(LOCK, "temperature", u32::MAX),
            Some(0xfedcba9876543210)
        );
        assert_eq!(locked_shape(LOCK, "temperature", 2), None);
        assert_eq!(locked_shape(LOCK, "other", 1), None);
        assert_eq!(locked_shape("version = 1\n", "temperature", 1), None);
        for end in 0..LOCK
            .find("\n\n[[contract]]\nname = \"temperature\"\nversion = 4294967295")
            .unwrap()
        {
            assert_eq!(locked_shape(&LOCK[..end], "temperature", 1), None);
        }
    }

    #[test]
    fn invalid_lock_fields_are_not_shapes() {
        for lock in [
            LOCK.replace("version = 1", "version = 2"),
            LOCK.replace("version = 1\nshape", "version = 4294967296\nshape"),
            LOCK.replace("abcdef", "abcdeg"),
            LOCK.replace("sha256:", "sha512:"),
        ] {
            assert_eq!(locked_shape(&lock, "temperature", 1), None);
        }
        assert_eq!(
            locked_shape(&LOCK.replace('\n', "\r\n"), "temperature", 1),
            SHAPE
        );
    }

    #[cfg(feature = "lock")]
    #[test]
    fn lookup_matches_lock_writer_escaping() {
        for name in [
            "plain",
            "with\"quote",
            "back\\slash",
            "line\nbreak",
            "tab\t",
            "\u{7f}\u{85}\u{9f}",
            "Wärme.温度",
        ] {
            let lock = LOCK.replace("\"temperature\"", &crate::lock::quote(name));
            assert_eq!(locked_shape(&lock, name, 1), SHAPE);
        }
    }

    #[test]
    fn known_ids_and_catalog_collision() {
        assert_eq!(name_id(""), 0x811c9dc5);
        assert_eq!(name_id("hello"), 0x4f9f2cab);
        assert_eq!(name_id("costarring"), name_id("liquid"));
        let catalog = [
            crate::ContractId {
                name: "temperature",
                version: 1,
                shape: None,
            },
            crate::ContractId {
                name: "temperature",
                version: 2,
                shape: SHAPE,
            },
        ];
        assert!(!has_name_id_collision(&catalog));
        let colliding = [
            crate::ContractId {
                name: "costarring",
                version: 1,
                shape: None,
            },
            crate::ContractId {
                name: "liquid",
                version: 1,
                shape: None,
            },
        ];
        assert!(has_name_id_collision(&colliding));
        #[cfg(feature = "aimdb-core")]
        for name in ["", "hello", "costarring", "Wärme.温度"] {
            assert_eq!(name_id(name), aimdb_core::envelope::name_id(name));
        }
        struct Uncatalogued;
        impl ContractShape for Uncatalogued {}
        assert_eq!(Uncatalogued::SHAPE, None);
    }
}
