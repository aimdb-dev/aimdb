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
///
/// A key that is itself a wildcard matches no pattern. Zenoh delivers a `put`
/// on `a/*` to every intersecting subscriber with `a/*` as the key, and no
/// record stands for it: `{cell}` would capture `*`.
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

/// Whether `a` matches every key `b` matches.
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
        // `a`'s pieces hold no `$` or `*`, so they can only land in `b`'s
        // text between its `$*`s, and each `$*` of `b` falls in one of `a`'s.
        (Chunk::Glob(x), Chunk::Glob(y)) => glob_matches(x, &y.join("$*")),
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
    /// Matches `segment`, which holds no `**`, chunk by chunk from byte `at`
    /// (`None`: the topic is used up). Returns where the topic continues, or
    /// `None` if the segment does not fit there.
    fn segment_at(
        segment: &[Chunk],
        topic: &str,
        mut at: Option<usize>,
        spans: &mut Spans,
    ) -> Option<Option<usize>> {
        for chunk in segment {
            let here = at?;
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
            if !fits {
                return None;
            }
            at = after;
        }
        Some(at)
    }
}

/// The router never passes more than `u16::MAX` bytes.
fn span(start: usize, end: usize) -> (u16, u16) {
    (start as u16, end as u16)
}

impl TopicFilter for ZenohFilter {
    fn filter(&self) -> &str {
        &self.filter
    }

    fn is_literal(&self) -> bool {
        self.chunks.iter().all(|c| matches!(c, Chunk::Literal(_)))
    }

    fn matches(&self, topic: &str, spans: &mut Spans) -> bool {
        // A wildcard key (`*`, `**` or `$*`): see `ZenohGrammar`.
        if topic.contains('*') {
            return false;
        }
        // The pattern is segments separated by `**`. Each `**` takes chunks
        // until the next segment fits, so segments land as early as they can:
        // linear in the topic, and the fewest chunks for each `{name..}` in
        // order. A `**` cannot take a verbatim chunk.
        let is_multi = |c: &Chunk| matches!(c, Chunk::Multi(_));
        let mut segments = self.chunks.split(is_multi).peekable();
        let mut multis = self.chunks.iter().filter_map(|c| match c {
            Chunk::Multi(capture) => Some(*capture),
            _ => None,
        });

        let start = (!topic.is_empty()).then_some(0);
        let first = segments.next().unwrap_or(&[]);
        let Some(mut at) = Self::segment_at(first, topic, start, spans) else {
            return false;
        };
        while let Some(segment) = segments.next() {
            let capture = multis.next().flatten();
            let last = segments.peek().is_none();
            let gap_start = at.unwrap_or(topic.len());
            let (mut gap_end, mut here) = (gap_start, at);
            loop {
                if let Some(n) = capture {
                    spans[n] = span(gap_start, gap_end);
                }
                match Self::segment_at(segment, topic, here, spans) {
                    // The last segment must also use up the topic.
                    Some(after) if !last || after.is_none() => {
                        at = after;
                        break;
                    }
                    _ => {}
                }
                let Some(p) = here else { return false };
                let (text, next) = chunk_at(topic, p);
                if text.starts_with('@') {
                    return false;
                }
                (gap_end, here) = (p + text.len(), next);
            }
        }
        at.is_none()
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
        for bad in ["a*", "a/**b", "a/#", "a/?", "a/$x", "a//b", "a/", "@*"] {
            assert!(compile(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn captures_must_be_whole_chunks() {
        for bad in [
            "dev-{id}",
            "a/{x}b",
            "a/{x}{y}",
            "a/{x}{y..}",
            "{x}$*",
            "@{x}",
        ] {
            assert!(compile(bad).is_err(), "{bad} should be refused");
        }
    }

    /// Zenoh accepts `@v$*`, but in a verbatim chunk `$*` is literal: it
    /// matches only the key `@v$*`. Refused as a likely mistake.
    #[test]
    fn rejects_wildcards_in_verbatim_chunks() {
        assert!(compile("@v$*").is_err());
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
    fn wildcard_keys_match_nothing() {
        for (pattern, key) in [
            ("aimdb/{cell}/state", "aimdb/*/state"),
            ("aimdb/{rest..}", "aimdb/**"),
            ("{x}", "c$*"),
            ("**", "a/*"),
        ] {
            assert_eq!(captures(pattern, key), None, "{pattern} on {key}");
        }
    }

    /// Keys come from remote publishers: a near-miss on a 64 KB key with
    /// several `**` must cost linear time, not one pass per way to split it.
    #[test]
    fn long_keys_match_in_linear_time() {
        let key = (0..32_000)
            .map(|i| if i % 2 == 0 { "x" } else { "y" })
            .collect::<Vec<_>>()
            .join("/");
        assert_eq!(captures("{a..}/x/{b..}/y/{c..}/z", &key), None);
        assert_eq!(captures("**/x/**/y/**/x/**/y/**/z", &key), None);
        let last = captures("{a..}/x/{b..}", &key).expect("matches");
        assert_eq!((last[0].as_str(), last[1].len()), ("", key.len() - 2));
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
        assert!(g.covers("x$*", "x$*y"));
        assert!(g.covers("$*a$*", "a$*b"));
        assert!(!g.covers("x$*y", "x$*"));
        assert!(!g.covers("$*ab$*", "$*a$*b$*"), "`b` also matches `axb`");
    }
}
