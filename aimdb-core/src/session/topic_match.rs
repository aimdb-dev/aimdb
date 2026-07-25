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

/// Returns `true` if every topic matched by `requested` is also matched by
/// `grant` — i.e. `grant`'s match set ⊇ `requested`'s match set (`grant`
/// *contains* `requested`).
///
/// This is **pattern containment**, not topic matching, and the two diverge
/// once `requested` is itself a wildcard. An access check must ask containment:
/// "does my grant cover the whole pattern this client asked to subscribe to?"
/// [`topic_matches`] answers a different question ("does this pattern match
/// this string?") and would treat the requested pattern as an opaque topic —
/// so a one-level grant `a.*` "matches" the all-levels request `a.#` (the `*`
/// swallows the `#`) and silently widens the grant. Containment denies that:
/// `a.#` reaches deeper than `a.*` covers.
///
/// When `requested` is a concrete topic (no wildcard) this collapses to exactly
/// [`topic_matches`]`(grant, requested)`, so it is a safe drop-in for an ACL
/// that must accept both concrete and wildcard subscription requests.
///
/// Grammar is the same dot-separated `*` (single-level) / `#` (multi-level, and
/// as `grant`'s tail, zero-or-more) set as [`topic_matches`].
pub fn pattern_contains(grant: &str, requested: &str) -> bool {
    let mut g = grant.split('.');
    let mut r = requested.split('.');
    loop {
        match (g.next(), r.next()) {
            // A `#` in the grant absorbs the entire remaining request tail
            // (including an already-exhausted one, e.g. `a.#` contains `a`).
            (Some("#"), _) => return true,
            // Both exhausted together: an exact structural cover.
            (None, None) => return true,
            // Request reaches past where the grant stops covering.
            (None, Some(_)) => return false,
            // Grant still requires ≥1 concrete segment the request never yields.
            (Some(_), None) => return false,
            // A grant `*` covers exactly one segment. A request `#` here could
            // expand to zero or many segments, so `*` cannot contain it; a
            // request `*` or literal is exactly one segment and is covered.
            (Some("*"), Some(rs)) => {
                if rs == "#" {
                    return false;
                }
            }
            // A grant literal covers only itself: the request segment must be
            // that same literal (a request `*`/`#` here would reach topics the
            // literal doesn't, so it is not contained).
            (Some(gs), Some(rs)) => {
                if gs != rs {
                    return false;
                }
            }
        }
    }
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

    #[test]
    fn containment_denies_wildcard_escalation() {
        // The bug this function exists to prevent: a one-level grant must not
        // cover an all-levels request just because `topic_matches` lets the
        // grant's `*` swallow the request's `#`.
        assert!(!pattern_contains("sensors.*", "sensors.#"));
        assert!(topic_matches("sensors.*", "sensors.#")); // the trap it avoids
                                                          // `*` also can't cover a deeper concrete request.
        assert!(!pattern_contains("sensors.*", "sensors.temp.vienna"));
    }

    #[test]
    fn containment_allows_when_grant_is_broader_or_equal() {
        // `#` covers everything.
        assert!(pattern_contains("#", "sensors.#"));
        assert!(pattern_contains("#", "anything.deep.here"));
        // A grant `#` tail covers deeper requests and the prefix itself.
        assert!(pattern_contains("sensors.#", "sensors.temp.#"));
        assert!(pattern_contains("sensors.#", "sensors.*"));
        assert!(pattern_contains("sensors.#", "sensors.temp"));
        assert!(pattern_contains("sensors.#", "sensors")); // `a.#` contains `a`
                                                           // Equal patterns contain each other.
        assert!(pattern_contains("sensors.#", "sensors.#"));
        assert!(pattern_contains("a.*.c", "a.*.c"));
        // One-level grant covers one-level requests (literal or `*`).
        assert!(pattern_contains("sensors.*", "sensors.temp"));
        assert!(pattern_contains("sensors.*", "sensors.*"));
    }

    #[test]
    fn containment_denies_out_of_scope_or_shallower() {
        // Different subtree.
        assert!(!pattern_contains("sensors.#", "commands.#"));
        assert!(!pattern_contains("sensors.temp", "sensors.humidity"));
        // A deeper grant does not cover a shallower request.
        assert!(!pattern_contains("a.b.c", "a.b"));
        // A literal grant is not widened by a `*`/`#` request.
        assert!(!pattern_contains("sensors.temp", "sensors.*"));
        assert!(!pattern_contains("sensors.temp", "sensors.#"));
    }

    #[test]
    fn containment_matches_topic_matches_on_concrete_requests() {
        // For a concrete (wildcard-free) request, containment must be exactly
        // `topic_matches` — the safe-drop-in property the ACL relies on.
        let grants = ["#", "sensors.#", "sensors.*", "sensors.temp", "*.temp"];
        let topics = [
            "sensors.temp",
            "sensors.temp.vienna",
            "sensors",
            "commands.on",
            "a.temp",
        ];
        for g in grants {
            for t in topics {
                assert_eq!(
                    pattern_contains(g, t),
                    topic_matches(g, t),
                    "grant={g:?} topic={t:?}"
                );
            }
        }
    }
}
