//! Topic pattern matching over **dot-separated** record keys (pure `&str` ops,
//! no_std-safe).
//!
//! Shared by every transport that supports wildcard subscriptions: the AimX
//! wildcard subscribe ([`crate::session::aimx`]) matches a pattern against the
//! registry once at subscribe time, and the WebSocket connector's fan-out bus
//! matches per broadcast.
//!
//! The one segment separator is `.` — AimDB record keys are dot-delimited
//! (`temp.vienna`, `app.config`), so wildcards split on `.`. The grammar
//! (dot segments, `*` single-level, `#` multi-level) is RabbitMQ topic-exchange
//! semantics. `/` is an ordinary character here — it belongs to external broker
//! addresses (`mqtt://sensors/temp/x`), not to AimDB's subscription grammar.

/// Returns `true` if `topic` matches `pattern`.
///
/// Wildcard conventions over **dot-separated** segments:
///
/// | Pattern  | Semantics                         |
/// |----------|-----------------------------------|
/// | `#`      | Multi-level wildcard (all topics) |
/// | `a.#`    | Everything under `a.`             |
/// | `a.*.c`  | Single-level wildcard in segment  |
/// | `a.b.c`  | Exact match                       |
pub fn topic_matches(pattern: &str, topic: &str) -> bool {
    // Fast path: exact match
    if pattern == topic {
        return true;
    }

    // Multi-level wildcard: `#` matches everything
    if pattern == "#" {
        return true;
    }

    // `prefix.#` matches everything under prefix — only when prefix is literal
    // (no wildcards in the prefix). When wildcards are present, fall through to
    // the segment loop which handles `#` at any position.
    if let Some(prefix) = pattern.strip_suffix(".#") {
        if !prefix.contains('*') && !prefix.contains('#') {
            return topic.starts_with(prefix)
                && (topic.len() == prefix.len()
                    || topic.as_bytes().get(prefix.len()) == Some(&b'.'));
        }
    }

    // Segment-by-segment matching with `*` single-level wildcard
    let mut pattern_parts = pattern.split('.');
    let mut topic_parts = topic.split('.');

    loop {
        match (pattern_parts.next(), topic_parts.next()) {
            (Some("#"), _) => return true,
            (Some("*"), Some(_)) => {} // single-level wildcard — consume one segment
            (Some(p), Some(t)) if p == t => {} // literal match
            (None, None) => return true, // both exhausted at the same time
            _ => return false,
        }
    }
}

/// Returns `true` if `pattern` contains a wildcard segment — i.e. subscribing
/// to it means "match against the registry" rather than "resolve one key".
pub fn is_wildcard(pattern: &str) -> bool {
    pattern.contains('#') || pattern.contains('*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match() {
        assert!(topic_matches("a.b.c", "a.b.c"));
        assert!(!topic_matches("a.b.c", "a.b.d"));
    }

    #[test]
    fn hash_wildcard() {
        assert!(topic_matches("#", "anything.goes.here"));
        assert!(topic_matches("#", "a"));
    }

    #[test]
    fn prefix_hash_wildcard() {
        assert!(topic_matches("sensors.#", "sensors.temperature.vienna"));
        assert!(topic_matches("sensors.#", "sensors.humidity.berlin"));
        assert!(!topic_matches("sensors.#", "commands.setpoint"));
        // Edge: prefix itself
        assert!(topic_matches("sensors.#", "sensors"));
        // A literal prefix is a whole segment — `sensors.#` must not swallow a
        // key that merely *starts with* the string "sensors".
        assert!(!topic_matches("sensors.#", "sensors_extra.temp"));
    }

    #[test]
    fn star_wildcard() {
        assert!(topic_matches(
            "sensors.temperature.*",
            "sensors.temperature.vienna"
        ));
        assert!(topic_matches(
            "sensors.temperature.*",
            "sensors.temperature.berlin"
        ));
        assert!(!topic_matches(
            "sensors.temperature.*",
            "sensors.humidity.vienna"
        ));
        assert!(!topic_matches(
            "sensors.temperature.*",
            "sensors.temperature.a.b"
        ));
    }

    #[test]
    fn star_matches_dotted_key_below_top_level() {
        // A single-segment-below wildcard must match a dot-separated key:
        // `temp.*` matches `temp.vienna`, not the old bug where `/`-splitting
        // compared the literals `"temp.*"` and `"temp.vienna"`.
        assert!(topic_matches("temp.*", "temp.vienna"));
        assert!(topic_matches("temp.*", "temp.berlin"));
        assert!(!topic_matches("temp.*", "temp"));
        assert!(!topic_matches("temp.*", "temp.vienna.indoor"));
        assert!(!topic_matches("temp.*", "humidity.vienna"));
    }

    #[test]
    fn mixed_wildcards() {
        assert!(topic_matches("a.*.c.#", "a.b.c.d.e.f"));
        assert!(!topic_matches("a.*.c.#", "a.b.x.d"));
    }

    #[test]
    fn slash_is_not_a_separator() {
        // `/` is an ordinary character — a slash key is one literal segment, so a
        // dot wildcard doesn't reach into it and a slash "wildcard" isn't one.
        assert!(!topic_matches("sensors.#", "sensors/temp/vienna"));
        assert!(topic_matches("sensors/temp", "sensors/temp")); // exact still works
        assert!(!is_wildcard("sensors/temp")); // no `#`/`*` → literal
    }

    #[test]
    fn wildcard_detection() {
        assert!(is_wildcard("#"));
        assert!(is_wildcard("sensors.#"));
        assert!(is_wildcard("a.*.c"));
        // Literal dotted keys are wildcards only when `#`/`*` appears.
        assert!(!is_wildcard("temp.vienna"));
        assert!(is_wildcard("temp.*"));
    }
}
