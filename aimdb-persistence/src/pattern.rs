//! Record-pattern support for backends.
//!
//! Patterns are MQTT-style over dot-separated record keys, the grammar
//! [`aimdb_core::topic_matches`] implements for subscriptions. A store can rarely
//! express "exactly one segment" natively, so backends narrow with
//! [`literal_prefix`] and leave the deciding to that matcher.

/// The pattern's leading literal, i.e. everything before its first wildcard
/// segment: a sound prefilter, since every key the pattern matches starts with
/// it. `""` when the pattern opens with a wildcard.
pub fn literal_prefix(pattern: &str) -> &str {
    match pattern.split('.').position(|seg| seg == "*" || seg == "#") {
        // Keep the `.` before the wildcard: `sensors.*` can only match keys under
        // `sensors.`, and including the separator keeps `sensors_extra.x` out.
        Some(0) => "",
        Some(n) => {
            let end = pattern
                .split('.')
                .take(n)
                .map(|seg| seg.len() + 1)
                .sum::<usize>();
            &pattern[..end]
        }
        None => pattern,
    }
}

/// Exclusive upper bound for a bytewise prefix scan: `["sensors.", "sensors/")`
/// holds every key starting with `sensors.`. `None` when no bound below
/// "everything" exists — an empty or all-`0xFF` prefix.
pub fn prefix_upper_bound(prefix: &str) -> Option<Vec<u8>> {
    let mut bound = prefix.as_bytes().to_vec();
    // Increment the last byte that can carry, dropping the saturated tail — the
    // successor of `a\xFF\xFF` is `b`.
    while let Some(last) = bound.pop() {
        if last < u8::MAX {
            bound.push(last + 1);
            return Some(bound);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_stops_at_the_first_wildcard() {
        assert_eq!(literal_prefix("sensors.*"), "sensors.");
        assert_eq!(literal_prefix("sensors.#"), "sensors.");
        assert_eq!(literal_prefix("a.b.*.d"), "a.b.");
        assert_eq!(literal_prefix("sensors.temp"), "sensors.temp");
    }

    /// A leading wildcard leaves nothing to narrow with.
    #[test]
    fn a_wildcard_first_segment_has_no_prefix() {
        assert_eq!(literal_prefix("#"), "");
        assert_eq!(literal_prefix("*"), "");
        assert_eq!(literal_prefix("*.temp"), "");
    }

    /// A wildcard is a whole segment: `sensors*` is a literal key, not a pattern.
    #[test]
    fn a_wildcard_must_be_its_own_segment() {
        assert_eq!(literal_prefix("sensors*"), "sensors*");
        assert_eq!(literal_prefix("sensors*.temp"), "sensors*.temp");
    }

    #[test]
    fn the_upper_bound_is_the_prefix_successor() {
        assert_eq!(prefix_upper_bound("sensors."), Some(b"sensors/".to_vec()));
        assert_eq!(prefix_upper_bound("a"), Some(b"b".to_vec()));
        assert_eq!(prefix_upper_bound(""), None);
    }

    /// The bound is over bytes, not chars: a multi-byte tail increments its last
    /// byte, which still orders after every key under the prefix.
    #[test]
    fn the_bound_is_bytewise() {
        // U+00FF encodes as `c3 bf`.
        assert_eq!(prefix_upper_bound("a\u{ff}"), Some(vec![b'a', 0xc3, 0xc0]));
    }
}
