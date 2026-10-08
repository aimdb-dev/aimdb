//! Zenoh key expressions as a [`TopicGrammar`].

use aimdb_core::{PatternPart, Spans, TopicFilter, TopicGrammar, TopicPattern};
use alloc::{boxed::Box, format, string::String, vec::Vec};

/// Zenoh key expressions: `/` separates chunks, `*` matches one chunk, `**`
/// any number of chunks (anywhere, including none), and `$*` inside a chunk
/// any run of characters. A chunk starting with `@` is verbatim: only the
/// same chunk matches it, never a wildcard.
///
/// `{name}` matches one chunk and `{name..}` any number; a capture is a whole
/// chunk. Where a pattern is ambiguous, a `{name..}` capture takes as few
/// chunks as it can. [`TopicFilter::filter`] is the canonical key expression
/// Zenoh accepts for a subscriber (`{a..}/{b}` subscribes `*/**`).
#[derive(Debug, Clone, Copy, Default)]
pub struct ZenohGrammar;

#[derive(Debug, PartialEq)]
enum Chunk {
    /// A plain or verbatim (`@…`) chunk.
    Literal(String),
    /// `*` or `{name}`, with the capture number.
    Single(Option<usize>),
    /// `**` or `{name..}`, with the capture number.
    Multi(Option<usize>),
    /// A chunk with `$*`: the text around each `$*`, in order. The first and
    /// last piece may be empty, the others are not.
    Glob(Vec<String>),
}

struct ZenohFilter {
    filter: String,
    chunks: Vec<Chunk>,
}

/// A chunk as written: its text and, for `{…}`, the capture number and
/// whether it is `{name..}`.
#[derive(Default)]
struct RawChunk {
    text: String,
    capture: Option<(usize, bool)>,
}

impl TopicGrammar for ZenohGrammar {
    fn compile(&self, pattern: &TopicPattern<'_>) -> Result<Box<dyn TopicFilter>, String> {
        let topic = pattern.as_str();

        let mut raw = Vec::from([RawChunk::default()]);
        let mut captures = 0;
        for part in pattern.parts() {
            match part {
                PatternPart::Text(text) => {
                    for (i, segment) in text.split('/').enumerate() {
                        if i > 0 {
                            raw.push(RawChunk::default());
                        }
                        if let Some(chunk) = raw.last_mut() {
                            chunk.text.push_str(segment);
                        }
                    }
                }
                PatternPart::Capture { multi, .. } => {
                    if let Some(chunk) = raw.last_mut() {
                        if chunk.capture.is_some() {
                            return Err(whole_chunk(topic));
                        }
                        chunk.capture = Some((captures, *multi));
                    }
                    captures += 1;
                }
            }
        }

        let mut chunks = Vec::with_capacity(raw.len());
        for RawChunk { text, capture } in raw {
            chunks.push(match capture {
                Some(_) if !text.is_empty() => return Err(whole_chunk(topic)),
                Some((n, false)) => Chunk::Single(Some(n)),
                Some((n, true)) => Chunk::Multi(Some(n)),
                None => classify(&text).map_err(|why| format!("'{topic}': {why}"))?,
            });
        }

        Ok(Box::new(ZenohFilter {
            filter: canonical(&chunks),
            chunks,
        }))
    }

    fn covers(&self, a: &str, b: &str) -> bool {
        match (parse(a), parse(b)) {
            (Some(a), Some(b)) => includes(&a, &b),
            _ => false,
        }
    }
}

fn whole_chunk(topic: &str) -> String {
    format!("'{topic}': a capture must be a whole chunk, not part of one")
}

/// One chunk without captures.
fn classify(text: &str) -> Result<Chunk, &'static str> {
    match text {
        "" => return Err("a key expression has no empty chunks"),
        "*" => return Ok(Chunk::Single(None)),
        "**" => return Ok(Chunk::Multi(None)),
        _ => {}
    }
    if text.contains(['#', '?']) {
        return Err("'#' and '?' are not allowed in a key expression");
    }
    if !text.contains(['*', '$']) {
        return Ok(Chunk::Literal(text.into()));
    }
    if text.starts_with('@') {
        return Err("a verbatim '@' chunk cannot hold a wildcard");
    }
    let mut pieces: Vec<String> = text.split("$*").map(String::from).collect();
    if pieces.iter().any(|p| p.contains(['*', '$'])) {
        return Err("'*' and '**' must be whole chunks; inside a chunk use '$*'");
    }
    // `$*$*` is `$*`: drop the empty pieces between them.
    let last = pieces.len() - 1;
    let mut i = 0;
    pieces.retain(|p| {
        let keep = i == 0 || i == last || !p.is_empty();
        i += 1;
        keep
    });
    if pieces.iter().all(String::is_empty) {
        Ok(Chunk::Single(None))
    } else {
        Ok(Chunk::Glob(pieces))
    }
}

/// The canonical key expression: within each run of `*` and `**` chunks, the
/// `*`s come first and one `**` ends it.
fn canonical(chunks: &[Chunk]) -> String {
    let mut out: Vec<String> = Vec::with_capacity(chunks.len());
    let (mut singles, mut multi) = (0, false);
    let flush = |out: &mut Vec<String>, singles: &mut usize, multi: &mut bool| {
        out.extend(core::iter::repeat_n(String::from("*"), *singles));
        if *multi {
            out.push(String::from("**"));
        }
        (*singles, *multi) = (0, false);
    };
    for chunk in chunks {
        match chunk {
            Chunk::Single(_) => singles += 1,
            Chunk::Multi(_) => multi = true,
            Chunk::Literal(text) => {
                flush(&mut out, &mut singles, &mut multi);
                out.push(text.clone());
            }
            Chunk::Glob(pieces) => {
                flush(&mut out, &mut singles, &mut multi);
                out.push(pieces.join("$*"));
            }
        }
    }
    flush(&mut out, &mut singles, &mut multi);
    out.join("/")
}

/// A canonical filter's chunks, or `None` if it is not one.
fn parse(filter: &str) -> Option<Vec<Chunk>> {
    filter.split('/').map(|c| classify(c).ok()).collect()
}

fn is_verbatim(chunk: &Chunk) -> bool {
    matches!(chunk, Chunk::Literal(text) if text.starts_with('@'))
}

/// Whether `a` matches every key `b` matches. Conservative for two `$*` chunks.
fn includes(a: &[Chunk], b: &[Chunk]) -> bool {
    match (a.split_first(), b.split_first()) {
        (None, None) => true,
        (Some((Chunk::Multi(_), a_rest)), _) => {
            includes(a_rest, b)
                || matches!(b.split_first(), Some((first, b_rest))
                    if !is_verbatim(first) && includes(a, b_rest))
        }
        (None, Some(_)) | (Some(_), None) | (Some(_), Some((Chunk::Multi(_), _))) => false,
        (Some((x, a_rest)), Some((y, b_rest))) => chunk_includes(x, y) && includes(a_rest, b_rest),
    }
}

/// Whether chunk `a` matches every chunk `b` matches; neither is `**`.
fn chunk_includes(a: &Chunk, b: &Chunk) -> bool {
    if is_verbatim(b) {
        return a == b;
    }
    match (a, b) {
        (Chunk::Literal(x), Chunk::Literal(y)) => x == y,
        (Chunk::Single(_), _) => true,
        (Chunk::Glob(pieces), Chunk::Literal(text)) => glob_matches(pieces, text),
        (Chunk::Glob(x), Chunk::Glob(y)) => x == y,
        _ => false,
    }
}

/// Whether `text` fits `pieces` joined by `$*`, each `$*` matching any run of
/// characters, including none.
fn glob_matches(pieces: &[String], text: &str) -> bool {
    let (Some((first, rest)), Some(last)) = (pieces.split_first(), pieces.last()) else {
        return false;
    };
    let Some(body) = text.strip_prefix(first.as_str()) else {
        return false;
    };
    let Some(mut body) = body.strip_suffix(last.as_str()) else {
        return false;
    };
    for middle in &rest[..rest.len().saturating_sub(1)] {
        match body.find(middle.as_str()) {
            Some(at) => body = &body[at + middle.len()..],
            None => return false,
        }
    }
    true
}

/// The chunk starting at byte `at`, and where the next one starts.
fn chunk_at(topic: &str, at: usize) -> (&str, Option<usize>) {
    match topic[at..].find('/') {
        Some(len) => (&topic[at..at + len], Some(at + len + 1)),
        None => (&topic[at..], None),
    }
}

impl ZenohFilter {
    /// Matches `chunks[from..]` against the topic from byte `at` (`None`: the
    /// topic is used up), backtracking over `**`.
    fn matches_from(&self, from: usize, topic: &str, at: Option<usize>, spans: &mut Spans) -> bool {
        // The router never passes more than `u16::MAX` bytes.
        let span = |start: usize, end: usize| (start as u16, end as u16);
        let Some(chunk) = self.chunks.get(from) else {
            return at.is_none();
        };

        if let Chunk::Multi(capture) = chunk {
            let start = at.unwrap_or(topic.len());
            let (mut end, mut next) = (start, at);
            loop {
                if let Some(n) = capture {
                    spans[*n] = span(start, end);
                }
                if self.matches_from(from + 1, topic, next, spans) {
                    return true;
                }
                let Some(here) = next else { return false };
                let (text, after) = chunk_at(topic, here);
                if text.starts_with('@') {
                    return false;
                }
                (end, next) = (here + text.len(), after);
            }
        }

        let Some(here) = at else { return false };
        let (text, after) = chunk_at(topic, here);
        let fits = match chunk {
            Chunk::Literal(literal) => literal == text,
            Chunk::Single(capture) => {
                if let Some(n) = capture {
                    spans[*n] = span(here, here + text.len());
                }
                !text.starts_with('@')
            }
            Chunk::Glob(pieces) => !text.starts_with('@') && glob_matches(pieces, text),
            Chunk::Multi(_) => false,
        };
        fits && self.matches_from(from + 1, topic, after, spans)
    }
}

impl TopicFilter for ZenohFilter {
    fn filter(&self) -> &str {
        &self.filter
    }

    fn is_literal(&self) -> bool {
        self.chunks.iter().all(|c| matches!(c, Chunk::Literal(_)))
    }

    fn matches(&self, topic: &str, spans: &mut Spans) -> bool {
        let start = (!topic.is_empty()).then_some(0);
        self.matches_from(0, topic, start, spans)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aimdb_core::MAX_CAPTURES;
    use alloc::string::ToString;
    use alloc::vec;

    fn compile(topic: &str) -> Result<Box<dyn TopicFilter>, String> {
        ZenohGrammar.compile(&TopicPattern::parse(topic).map_err(|e| e.to_string())?)
    }

    /// The captured text of each capture, or `None` if `topic` does not match.
    fn captures(pattern: &str, topic: &str) -> Option<Vec<String>> {
        let filter = compile(pattern).expect("compiles");
        let mut spans = [(0, 0); MAX_CAPTURES];
        let count = TopicPattern::parse(pattern)
            .ok()?
            .parts()
            .iter()
            .filter(|p| matches!(p, PatternPart::Capture { .. }))
            .count();
        filter.matches(topic, &mut spans).then(|| {
            spans[..count]
                .iter()
                .map(|&(s, e)| topic[usize::from(s)..usize::from(e)].into())
                .collect()
        })
    }

    #[test]
    fn rejects_what_zenoh_rejects() {
        for bad in [
            "dev-{id}", "a/{x}b", "a*", "a/**b", "a/#", "a/?", "a/$x", "a//b", "a/", "@v$*", "@*",
        ] {
            assert!(compile(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn filter_is_canonical() {
        for (pattern, filter) in [
            ("aimdb/{cell}/state", "aimdb/*/state"),
            ("{rest..}/{last}", "*/**"),
            ("a/**/**/b", "a/**/b"),
            ("a/**/*/*/b", "a/*/*/**/b"),
            ("a/$*/b", "a/*/b"),
            ("a/x$*$*y", "a/x$*y"),
            ("@ros2_lv/**", "@ros2_lv/**"),
        ] {
            assert_eq!(
                compile(pattern).expect(pattern).filter(),
                filter,
                "{pattern}"
            );
        }
    }

    #[test]
    fn literal_patterns_are_exact_routes() {
        assert!(compile("a/@v/b").expect("literal").is_literal());
        assert!(!compile("a/*").expect("wildcard").is_literal());
        assert!(!compile("a/x$*").expect("glob").is_literal());
    }

    #[test]
    fn captures_land_on_their_chunks() {
        assert_eq!(
            captures("aimdb/{cell}/state", "aimdb/cell4/state"),
            Some(vec!["cell4".into()])
        );
        assert_eq!(
            captures("{site}/{rest..}", "plant/hall/cell4"),
            Some(vec!["plant".into(), "hall/cell4".into()])
        );
        assert_eq!(captures("a/{x..}/b", "a/b"), Some(vec!["".into()]));
        assert_eq!(captures("a/{x..}", "a"), Some(vec!["".into()]));
        assert_eq!(
            captures("{x..}/c/{y..}", "c/c/c"),
            Some(vec!["".into(), "c/c".into()]),
            "a multi-chunk capture takes as few chunks as it can"
        );
        assert_eq!(captures("aimdb/{cell}/state", "aimdb/cell4"), None);
    }

    #[test]
    fn wildcards_skip_verbatim_chunks() {
        assert_eq!(captures("**", "@ros2_lv/0"), None);
        assert_eq!(captures("*/0", "@ros2_lv/0"), None);
        assert_eq!(captures("a/**", "a/@v"), None);
        assert_eq!(captures("@ros2_lv/**", "@ros2_lv/0/x"), Some(vec![]));
        assert_eq!(captures("a/@v", "a/@v"), Some(vec![]));
    }

    #[test]
    fn sub_chunk_wildcard_matches_any_run() {
        assert!(captures("a/x$*", "a/x").is_some());
        assert!(captures("a/x$*", "a/xyz").is_some());
        assert!(captures("a/$*z", "a/xyz").is_some());
        assert!(captures("a/x$*y$*z", "a/xyz").is_some());
        assert!(captures("a/x$*z", "a/xy").is_none());
        assert!(captures("a/x$*x", "a/x").is_none());
    }

    #[test]
    fn covers_follows_key_expression_inclusion() {
        let g = ZenohGrammar;
        assert!(g.covers("aimdb/*/state", "aimdb/cell4/state"));
        assert!(g.covers("a/**", "a"));
        assert!(g.covers("a/**", "a/*/b"));
        assert!(g.covers("**", "a/**/b"));
        assert!(g.covers("a/x$*", "a/xyz"));
        assert!(g.covers("*", "x$*"));
        assert!(!g.covers("a/*", "a/**"));
        assert!(!g.covers("a/*/c", "a/b/*"));
        assert!(!g.covers("**", "@ros2_lv/0"));
        assert!(!g.covers("*", "@v"));
        assert!(g.covers("@ros2_lv/**", "@ros2_lv/0/x"));
        assert!(!g.covers("x$*", "x$*y"), "conservative for two `$*` chunks");
    }
}
