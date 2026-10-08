//! `ZenohGrammar` against `zenoh-keyexpr` on a generated corpus: validation,
//! canonical filters, matching and covering agree with Zenoh's own rules.
//! Capture values, which Zenoh has no notion of, are checked against a
//! brute-force model.

use aimdb_core::{PatternPart, Spans, TopicFilter, TopicGrammar, TopicPattern, MAX_CAPTURES};
use aimdb_zenoh_connector::ZenohGrammar;
use zenoh_keyexpr::{keyexpr, OwnedKeyExpr};

const PATTERN_CHUNKS: [&str; 8] = ["a", "b", "@v", "*", "**", "a$*", "$*b", "a$*b"];
const KEY_CHUNKS: [&str; 4] = ["a", "b", "ab", "@v"];
const CAPTURE_PATTERNS: [&str; 12] = [
    "{x}",
    "{x..}",
    "a/{x..}/b",
    "{x}/{y..}",
    "{x..}/{y}",
    "@v/{x..}",
    "{x..}/a/{y..}",
    "{x..}/{y..}",
    "{x}/**/{y}",
    "**/{x}/*",
    "a$*/{x..}/b",
    "{x..}/@v/{y}",
];

/// Every `/`-joined sequence of 1 to `max` `chunks`.
fn sequences(chunks: &[&str], max: usize) -> Vec<String> {
    let mut out: Vec<String> = chunks.iter().map(|c| c.to_string()).collect();
    let mut previous = out.clone();
    for _ in 1..max {
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
    let mut all = sequences(&PATTERN_CHUNKS, 3);
    all.extend(CAPTURE_PATTERNS.iter().map(|p| p.to_string()));
    all
}

/// Every string over `a@$*/#` up to 6 bytes: `compile` accepts what Zenoh
/// accepts, except `$*` in an `@` chunk, and its filter is Zenoh's canon.
///
/// `zenoh-keyexpr` 1.10.1's canonizer panics on a run of `$*` ending the key
/// (`a$*$*`) and leaves `$*$*` from `$*$*$*`, so for those inputs only the
/// validity of our filter is checked.
#[test]
fn compile_agrees_with_zenoh_validation() {
    let mut all: Vec<String> = vec![String::new()];
    let mut checked = 0;
    for _ in 0..6 {
        all = all
            .iter()
            .flat_map(|s| "a@$*/#".chars().map(move |c| format!("{s}{c}")))
            .collect();
        for s in &all {
            let ours = ZenohGrammar.compile(&TopicPattern::parse(s).expect("no braces"));
            if s.contains("$*$*") {
                if let Ok(f) = &ours {
                    assert!(keyexpr::new(f.filter()).is_ok(), "{s}");
                }
                continue;
            }
            checked += 1;
            match (ours, OwnedKeyExpr::autocanonize(s.clone())) {
                (Ok(f), Ok(zenoh)) => assert_eq!(f.filter(), zenoh.as_str(), "{s}"),
                (Ok(f), Err(e)) => panic!("{s}: we accept ({}), Zenoh refuses: {e}", f.filter()),
                (Err(e), Ok(_)) => assert!(
                    s.split('/').any(|c| c.starts_with('@') && c.contains("$*")),
                    "{s}: Zenoh accepts, we refuse: {e}"
                ),
                (Err(_), Err(_)) => {}
            }
        }
    }
    assert!(checked > 50_000, "{checked} strings checked");
}

#[test]
fn filters_are_what_zenoh_canonizes_to() {
    for pattern in sequences(&PATTERN_CHUNKS, 3) {
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
    let keys = sequences(&KEY_CHUNKS, 3);
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

/// A `put` on a wildcard key expression arrives with the wildcard as its
/// key. Zenoh intersects it with subscribers; the grammar matches it with
/// nothing (see `ZenohGrammar`).
#[test]
fn wildcard_keys_match_no_pattern() {
    let keys: Vec<String> = sequences(&["a", "@v", "*", "**", "a$*"], 3)
        .into_iter()
        .filter(|k| k.contains('*') && keyexpr::new(k.as_str()).is_ok())
        .collect();
    let mut spans: Spans = [(0, 0); MAX_CAPTURES];
    let mut intersecting = 0;
    for pattern in patterns() {
        let filter = compile(&pattern);
        let zenoh = keyexpr::new(filter.filter()).expect("canonical");
        for key in &keys {
            intersecting += usize::from(zenoh.intersects(keyexpr::new(key.as_str()).unwrap()));
            assert!(!filter.matches(key, &mut spans), "{pattern} on {key}");
        }
    }
    assert!(intersecting > 1_000, "Zenoh would deliver {intersecting}");
}

#[test]
fn covering_equals_zenoh_includes() {
    let filters: Vec<String> = patterns()
        .iter()
        .map(|p| compile(p).filter().to_string())
        .collect();
    let (mut pairs, mut covered, mut globs) = (0, 0, 0);
    for a in &filters {
        let za = keyexpr::new(a.as_str()).expect("canonical");
        for b in &filters {
            let zb = keyexpr::new(b.as_str()).expect("canonical");
            let ours = ZenohGrammar.covers(a, b);
            assert_eq!(ours, za.includes(zb), "{a} covers {b}");
            pairs += 1;
            covered += usize::from(ours);
            globs += usize::from(ours && a != b && a.contains('$') && b.contains('$'));
        }
    }
    assert!(pairs > 100_000, "{pairs} pairs");
    assert!(
        covered > 1_000 && globs > 100,
        "{covered} covered, {globs} by a `$*` chunk"
    );
}

// ------------------------------------------------------- capture values

/// One chunk of a pattern as written, read without `ZenohGrammar`.
enum Model {
    Literal(String),
    One(Option<usize>),
    Many(Option<usize>),
    Glob(String),
}

fn model(pattern: &str) -> Vec<Model> {
    let mut captures = 0;
    pattern
        .split('/')
        .map(|chunk| {
            if chunk.starts_with('{') {
                captures += 1;
                if chunk.ends_with("..}") {
                    Model::Many(Some(captures - 1))
                } else {
                    Model::One(Some(captures - 1))
                }
            } else if chunk == "**" {
                Model::Many(None)
            } else if chunk == "*" {
                Model::One(None)
            } else if chunk.contains("$*") {
                Model::Glob(chunk.to_string())
            } else {
                Model::Literal(chunk.to_string())
            }
        })
        .collect()
}

/// Byte by byte: `$*` is any run, everything else itself.
fn glob(pattern: &[u8], text: &[u8]) -> bool {
    match pattern {
        [] => text.is_empty(),
        [b'$', b'*', rest @ ..] => (0..=text.len()).any(|i| glob(rest, &text[i..])),
        [c, rest @ ..] => text.first() == Some(c) && glob(rest, &text[1..]),
    }
}

/// Every way `pattern` matches `key`: per pattern chunk, the chunks it took.
fn assignments(pattern: &[Model], key: &[&str], taken: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
    let Some((first, rest)) = pattern.split_first() else {
        if key.is_empty() {
            out.push(taken.clone());
        }
        return;
    };
    let verbatim = |c: &&str| c.starts_with('@');
    if let Model::Many(_) = first {
        for n in 0..=key.len() {
            if key[..n].iter().any(verbatim) {
                break;
            }
            taken.push(n);
            assignments(rest, &key[n..], taken, out);
            taken.pop();
        }
        return;
    }
    let Some((chunk, key_rest)) = key.split_first() else {
        return;
    };
    let fits = match first {
        Model::Literal(text) => text == chunk,
        Model::One(_) => !verbatim(chunk),
        Model::Glob(g) => !verbatim(chunk) && glob(g.as_bytes(), chunk.as_bytes()),
        Model::Many(_) => unreachable!(),
    };
    if fits {
        taken.push(1);
        assignments(rest, key_rest, taken, out);
        taken.pop();
    }
}

/// The capture values when the leftmost capture takes the fewest chunks.
fn expected(pattern: &str, key: &str) -> Option<Vec<String>> {
    let model = model(pattern);
    let chunks: Vec<&str> = key.split('/').collect();
    let mut all = Vec::new();
    assignments(&model, &chunks, &mut Vec::new(), &mut all);
    let best = all.into_iter().min()?;
    let mut values = Vec::new();
    let mut at = 0;
    for (chunk, &n) in model.iter().zip(&best) {
        if let Model::One(Some(_)) | Model::Many(Some(_)) = chunk {
            values.push(chunks[at..at + n].join("/"));
        }
        at += n;
    }
    Some(values)
}

#[test]
fn captures_take_the_fewest_chunks_leftmost_first() {
    let keys = sequences(&["a", "b", "ab", "@v"], 4);
    let mut matched = 0;
    for pattern in CAPTURE_PATTERNS {
        let filter = compile(pattern);
        let count = TopicPattern::parse(pattern)
            .unwrap()
            .parts()
            .iter()
            .filter(|p| matches!(p, PatternPart::Capture { .. }))
            .count();
        for key in &keys {
            // Unwritten spans would read as `<unwritten>`.
            let mut spans: Spans = [(u16::MAX, u16::MAX); MAX_CAPTURES];
            let ours = filter.matches(key, &mut spans).then(|| {
                spans[..count]
                    .iter()
                    .map(|&(s, e)| {
                        key.get(usize::from(s)..usize::from(e))
                            .unwrap_or("<unwritten>")
                            .to_string()
                    })
                    .collect::<Vec<_>>()
            });
            matched += usize::from(ours.is_some());
            assert_eq!(ours, expected(pattern, key), "{pattern} on {key}");
        }
    }
    assert!(matched > 500, "{matched} matched");
}
