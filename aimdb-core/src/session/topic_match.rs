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
//! semantics: `#` absorbs **zero or more** segments and the pattern continues
//! after it, so a `#` in the middle still has to line its suffix up. `/` is an
//! ordinary character here — it belongs to external broker addresses
//! (`mqtt://sensors/temp/x`), not to AimDB's subscription grammar.

use core::str::Split;

/// Returns `true` if `topic` matches `pattern`.
///
/// Wildcard conventions over **dot-separated** segments:
///
/// | Pattern  | Semantics                               |
/// |----------|-----------------------------------------|
/// | `#`      | Multi-level wildcard (all topics)       |
/// | `a.#`    | `a` and everything under `a.`           |
/// | `a.#.c`  | `a.c` and any `c` at any depth under it |
/// | `a.*.c`  | Single-level wildcard in segment        |
/// | `a.b.c`  | Exact match                             |
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
    // (no wildcards in the prefix). Same answer as the general walk below, just
    // without splitting: this is the shape the WS fan-out matches per broadcast.
    if let Some(prefix) = pattern.strip_suffix(".#") {
        if !prefix.contains('*') && !prefix.contains('#') {
            return topic.starts_with(prefix)
                && (topic.len() == prefix.len()
                    || topic.as_bytes().get(prefix.len()) == Some(&b'.'));
        }
    }

    // A topic segment is always a literal, so a pattern segment covers it when
    // it is that same literal or the single-level wildcard.
    match_segments(pattern, topic, |p, t| p == "*" || p == t)
}

/// Segment walk shared by [`topic_matches`] and [`pattern_contains`]: the two
/// differ only in when one `pattern` segment covers one `subject` segment, which
/// is what `covers` decides. `#` is handled here because it is the one construct
/// that spans segments.
///
/// `#` absorbs **zero or more** subject segments and the pattern *continues*
/// after it — it is not a "match everything from here" escape hatch. That
/// distinction is load-bearing: returning `true` at the `#` would make the
/// suffix of `a.#.secret` dead text and let it cover `a.public`.
///
/// Only the most recent `#` is retained as a backtrack point. That is the
/// standard glob trick, and it is what bounds this at `O(|pattern| · |subject|)`
/// rather than exponential — worth keeping deliberate, since `subject` is the
/// client-supplied pattern on the [`pattern_contains`] ACL path.
fn match_segments<F>(pattern: &str, subject: &str, covers: F) -> bool
where
    F: Fn(&str, &str) -> bool,
{
    let mut pat = pattern.split('.');
    let mut sub = subject.split('.');
    // The pattern tail following the most recent `#`, paired with the subject
    // position that `#` currently stops absorbing at. `None` until a `#` is seen.
    let mut resume: Option<(Split<'_, char>, Split<'_, char>)> = None;

    loop {
        let mut pat_rest = pat.clone();
        let mut sub_rest = sub.clone();
        match (pat_rest.next(), sub_rest.next()) {
            // `#` starts out absorbing nothing; `resume` is how it grows.
            (Some("#"), _) => {
                pat = pat_rest;
                resume = Some((pat.clone(), sub.clone()));
            }
            (Some(p), Some(s)) if covers(p, s) => {
                pat = pat_rest;
                sub = sub_rest;
            }
            // Both exhausted together — every segment was covered.
            (None, None) => return true,
            // Mismatch, or one side ran out while the other still has segments.
            // The only way forward is to let the most recent `#` swallow one
            // more subject segment and retry its suffix from there.
            _ => {
                let Some((pat_after_hash, sub_at_hash)) = resume.take() else {
                    return false; // no `#` to fall back on
                };
                let mut grown = sub_at_hash;
                if grown.next().is_none() {
                    return false; // `#` has nothing left to absorb
                }
                pat = pat_after_hash.clone();
                sub = grown.clone();
                resume = Some((pat_after_hash, grown));
            }
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
/// Grammar is the same dot-separated `*` (single-level) / `#` (multi-level,
/// zero-or-more) set as [`topic_matches`]. A grant's `#` absorbs zero or more
/// *requested* segments and the grant continues after it, so an interior `#`
/// constrains rather than blanket-allows: `a.#.secret` contains `a.b.secret`
/// but not `a.public`.
///
/// # Conservative by construction
///
/// This decides containment with a single left-to-right walk, which is sound
/// (it never reports containment that does not hold) but not complete. Deciding
/// the remaining cases means reasoning about every expansion of the *request's*
/// `#`, and that ∀-branch is exactly the intricate, blowup-prone code an ACL on
/// client-supplied input should not carry.
///
/// The gap is confined to a grant whose `*` meets a request's `#`, where a later
/// segment would have ruled out the short expansions — e.g. `a.*.#` does contain
/// `a.#.a` (every topic the request admits has ≥2 segments), but this returns
/// `false`. Such a pair denies access that could have been allowed; it never
/// allows access that should have been denied. Grants without a `*`/`#`
/// adjacency — every ordinary shape, including `a.#`, `a.*`, `a.#.b`, `#` — are
/// decided exactly.
pub fn pattern_contains(grant: &str, requested: &str) -> bool {
    match_segments(grant, requested, |g, r| {
        if g == "*" {
            // A grant `*` covers exactly one segment. A request `#` could expand
            // to zero or many, so `*` cannot contain it; a request `*` or literal
            // is exactly one segment and is covered.
            r != "#"
        } else {
            // A grant literal covers only itself: the request segment must be
            // that same literal (a request `*`/`#` here would reach topics the
            // literal doesn't). A grant `#` never arrives here — the walk owns it.
            g == r
        }
    })
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
    fn non_terminal_hash_still_matches_its_suffix() {
        // Regression: `#` used to return `true` on sight, which made every
        // segment after it dead text — `a.#.secret` matched *anything* under
        // `a`. It absorbs zero or more segments and the suffix must still line up.
        assert!(!topic_matches("a.#.secret", "a.public"));
        assert!(!topic_matches("a.#.secret", "a.b.c.public"));
        assert!(!topic_matches("a.#.secret", "b.secret")); // prefix still binds
        assert!(!topic_matches("a.#.secret", "a.secret.tail"));
        // Zero-or-more: the `#` may absorb nothing, one segment, or many.
        assert!(topic_matches("a.#.secret", "a.secret"));
        assert!(topic_matches("a.#.secret", "a.b.secret"));
        assert!(topic_matches("a.#.secret", "a.b.c.d.secret"));
    }

    #[test]
    fn hash_backtracks_to_find_a_later_suffix() {
        // The suffix appears twice: the `#` has to keep growing past the first
        // occurrence to line the pattern up on the last one.
        assert!(topic_matches("a.#.c", "a.c.b.c"));
        assert!(!topic_matches("a.#.c", "a.c.b.d"));
        // Several `#` in one pattern each need their own extension.
        assert!(topic_matches("a.#.b.#.c", "a.x.y.b.z.c"));
        assert!(!topic_matches("a.#.b.#.c", "a.x.y.b.z.d"));
        // A trailing `#` after an interior one still absorbs the remainder.
        assert!(topic_matches("a.#.b.#", "a.x.b.y.z"));
        assert!(topic_matches("a.#.b.#", "a.b")); // both absorb zero
        assert!(!topic_matches("a.#.b.#", "a.x.y"));
    }

    #[test]
    fn hash_does_not_match_a_shorter_topic_than_its_suffix() {
        // `#` absorbing zero segments must not let the pattern outrun the topic.
        assert!(!topic_matches("a.#.b.c", "a.b"));
        assert!(!topic_matches("#.a", "b"));
        assert!(topic_matches("#.a", "a")); // leading `#` absorbs zero
        assert!(topic_matches("#.a", "x.y.a"));
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
    fn containment_honours_a_non_terminal_hash_in_the_grant() {
        // Regression: the grant's `#` used to return `true` on sight, so a grant
        // written to reach only `secret` leaves widened the ACL to the whole
        // subtree. The suffix after the `#` must still constrain the request.
        assert!(!pattern_contains("a.#.secret", "a.public"));
        assert!(!pattern_contains("a.#.secret", "a.b.public"));
        assert!(!pattern_contains("a.#.secret", "a.#")); // no escalation to the subtree
        assert!(!pattern_contains("a.#.secret", "a.*"));
        // What the grant genuinely covers stays covered.
        assert!(pattern_contains("a.#.secret", "a.secret"));
        assert!(pattern_contains("a.#.secret", "a.b.secret"));
        assert!(pattern_contains("a.#.secret", "a.#.secret"));
        assert!(pattern_contains("a.#.secret", "a.b.#.secret"));
    }

    #[test]
    fn containment_denies_requests_reaching_past_an_interior_hash() {
        // A request `#` where the grant demands a concrete tail is unbounded and
        // must be denied — it would reach topics the grant's suffix excludes.
        assert!(!pattern_contains("a.#.b", "a.#"));
        assert!(!pattern_contains("a.#.b", "#"));
        assert!(!pattern_contains("a.#.b.c", "a.b"));
        // A `*` cannot stand in for a request `#`, before or after a grant `#`.
        assert!(!pattern_contains("a.#.*", "a.#"));
        assert!(pattern_contains("a.#.*", "a.b.c"));
        assert!(!pattern_contains("a.#.*", "a"));
    }

    #[test]
    fn containment_is_conservative_where_a_grant_star_meets_a_request_hash() {
        // Pins the documented soundness/completeness trade-off so a future change
        // has to be deliberate. `a.*.#` genuinely contains `a.#.a` — every topic
        // the request admits has ≥2 segments, which is exactly what `*.#`
        // requires — but deciding that needs reasoning over every expansion of
        // the request's `#`, which this walk does not do. It denies instead.
        assert!(!pattern_contains("a.*.#", "a.#.a"));
        assert!(!pattern_contains("a.#.*", "a.b.#"));
        // The direction that matters: the denial is fail-closed, so no topic the
        // request admits is reachable in a way the grant would not have allowed.
        // (Sanity: the grant really is the broader pattern here.)
        for topic in ["a.a", "a.b.a", "a.b.c.a"] {
            assert!(topic_matches("a.#.a", topic));
            assert!(topic_matches("a.*.#", topic));
        }
        // Ordinary grants — no `*`/`#` adjacency — stay exact, not conservative.
        assert!(pattern_contains("a.#", "a.b.#"));
        assert!(pattern_contains("a.#.b", "a.x.b"));
        assert!(pattern_contains("a.*", "a.b"));
    }

    #[test]
    fn containment_matches_topic_matches_on_concrete_requests() {
        // For a concrete (wildcard-free) request, containment must be exactly
        // `topic_matches` — the safe-drop-in property the ACL relies on.
        let grants = [
            "#",
            "sensors.#",
            "sensors.*",
            "sensors.temp",
            "*.temp",
            // Interior `#` — the shape that used to short-circuit both walks.
            "sensors.#.temp",
            "#.temp",
            "sensors.#.*",
            "a.#.b.#.c",
        ];
        let topics = [
            "sensors.temp",
            "sensors.temp.vienna",
            "sensors",
            "commands.on",
            "a.temp",
            "sensors.rack.temp",
            "sensors.rack.shelf.temp",
            "a.x.b.y.c",
            "a.b.c",
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
