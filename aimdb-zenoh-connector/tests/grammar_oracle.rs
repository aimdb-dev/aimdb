//! `ZenohGrammar` against `zenoh-keyexpr` on a generated corpus: canonical
//! filters, matching and covering agree with Zenoh's own rules.

use aimdb_core::{Spans, TopicFilter, TopicGrammar, TopicPattern, MAX_CAPTURES};
use aimdb_zenoh_connector::ZenohGrammar;
use zenoh_keyexpr::{keyexpr, OwnedKeyExpr};

const PATTERN_CHUNKS: [&str; 7] = ["a", "b", "@v", "*", "**", "a$*", "$*b"];
const KEY_CHUNKS: [&str; 4] = ["a", "b", "ab", "@v"];
const CAPTURE_PATTERNS: [&str; 6] = [
    "{x}",
    "{x..}",
    "a/{x..}/b",
    "{x}/{y..}",
    "{x..}/{y}",
    "@v/{x..}",
];

/// Every `/`-joined sequence of 1 to 3 `chunks`.
fn sequences(chunks: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = chunks.iter().map(|c| c.to_string()).collect();
    let mut previous = out.clone();
    for _ in 1..3 {
        previous = previous
            .iter()
            .flat_map(|p| chunks.iter().map(move |c| format!("{p}/{c}")))
            .collect();
        out.extend(previous.iter().cloned());
    }
    out
}

fn compile(pattern: &str) -> Box<dyn TopicFilter> {
    let parsed = TopicPattern::parse(pattern).expect("valid `{…}` syntax");
    ZenohGrammar.compile(&parsed).expect(pattern)
}

fn patterns() -> Vec<String> {
    let mut all = sequences(&PATTERN_CHUNKS);
    all.extend(CAPTURE_PATTERNS.iter().map(|p| p.to_string()));
    all
}

#[test]
fn filters_are_what_zenoh_canonizes_to() {
    for pattern in sequences(&PATTERN_CHUNKS) {
        let filter = compile(&pattern);
        let zenoh = OwnedKeyExpr::autocanonize(pattern.clone()).expect("zenoh accepts it");
        assert_eq!(filter.filter(), zenoh.as_str(), "{pattern}");
        assert!(keyexpr::new(filter.filter()).is_ok(), "{pattern}");
    }
    for pattern in CAPTURE_PATTERNS {
        let filter = compile(pattern);
        assert!(keyexpr::new(filter.filter()).is_ok(), "{pattern}");
    }
}

#[test]
fn matching_agrees_with_zenoh() {
    let keys = sequences(&KEY_CHUNKS);
    let mut spans: Spans = [(0, 0); MAX_CAPTURES];
    let (mut matched, mut missed) = (0, 0);
    for pattern in patterns() {
        let filter = compile(&pattern);
        let zenoh = keyexpr::new(filter.filter()).expect("canonical");
        for key in &keys {
            let expected = zenoh.intersects(keyexpr::new(key.as_str()).expect("key"));
            if expected {
                matched += 1;
            } else {
                missed += 1;
            }
            assert_eq!(
                filter.matches(key, &mut spans),
                expected,
                "pattern {pattern} ({}) on key {key}",
                filter.filter()
            );
        }
    }
    assert!(
        matched > 1_000 && missed > 1_000,
        "{matched} matched, {missed} missed"
    );
}

#[test]
fn covering_is_sound_and_exact_without_sub_chunk_wildcards() {
    let filters: Vec<String> = patterns()
        .iter()
        .map(|p| compile(p).filter().to_string())
        .collect();
    let mut exact = 0;
    for a in &filters {
        let za = keyexpr::new(a.as_str()).expect("canonical");
        for b in &filters {
            let zb = keyexpr::new(b.as_str()).expect("canonical");
            let ours = ZenohGrammar.covers(a, b);
            let zenoh = za.includes(zb);
            assert!(!ours || zenoh, "{a} claimed to cover {b}");
            if !a.contains('$') && !b.contains('$') {
                assert_eq!(ours, zenoh, "{a} covers {b}");
                exact += 1;
            }
        }
    }
    assert!(exact > 10_000, "the exact comparison ran ({exact} pairs)");
}
