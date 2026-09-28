//! Inbound topic patterns: `{name}` captures one level, `{name..}` the rest.
//! Matching belongs to the connector's [`TopicGrammar`].

use alloc::{boxed::Box, format, string::String, vec::Vec};
use core::fmt;

use crate::inbound_key::KeyId;

/// Most captures one pattern may name.
pub const MAX_CAPTURES: usize = 8;

/// Byte range of each capture in the topic.
pub type Spans = [(u16, u16); MAX_CAPTURES];

/// Invalid `{…}` syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternError(String);

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One piece of a [`TopicPattern`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternPart<'a> {
    Text(&'a str),
    /// `{name}`, or `{name..}` when `multi`.
    Capture {
        name: &'a str,
        multi: bool,
    },
}

/// A topic with valid `{…}` syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicPattern<'a> {
    topic: &'a str,
    parts: Vec<PatternPart<'a>>,
}

impl<'a> TopicPattern<'a> {
    /// Rejects unbalanced braces, names outside `[A-Za-z0-9_]+`, duplicates
    /// and more than [`MAX_CAPTURES`] captures.
    pub fn parse(topic: &'a str) -> Result<Self, PatternError> {
        let mut parts = Vec::new();
        let mut captures = 0;
        let mut rest = topic;

        while let Some(open) = rest.find(['{', '}']) {
            if rest[open..].starts_with('}') {
                return Err(PatternError(format!("unbalanced '}}' in '{topic}'")));
            }
            let after = &rest[open + 1..];
            let close = match after.find(['{', '}']) {
                Some(i) if after[i..].starts_with('}') => i,
                _ => return Err(PatternError(format!("unbalanced '{{' in '{topic}'"))),
            };
            let body = &after[..close];
            let (name, multi) = match body.strip_suffix("..") {
                Some(name) => (name, true),
                None => (body, false),
            };
            if name.is_empty() {
                return Err(PatternError(format!("empty capture name in '{topic}'")));
            }
            if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return Err(PatternError(format!(
                    "capture name '{name}' in '{topic}' may only contain A-Z, a-z, 0-9 and _"
                )));
            }
            if parts
                .iter()
                .any(|p| matches!(p, PatternPart::Capture { name: n, .. } if *n == name))
            {
                return Err(PatternError(format!(
                    "capture '{name}' appears twice in '{topic}'"
                )));
            }
            if captures == MAX_CAPTURES {
                return Err(PatternError(format!(
                    "'{topic}' has more than {MAX_CAPTURES} captures"
                )));
            }
            if open > 0 {
                parts.push(PatternPart::Text(&rest[..open]));
            }
            parts.push(PatternPart::Capture { name, multi });
            captures += 1;
            rest = &after[close + 1..];
        }
        if !rest.is_empty() {
            parts.push(PatternPart::Text(rest));
        }

        Ok(Self { topic, parts })
    }

    pub fn as_str(&self) -> &'a str {
        self.topic
    }

    /// Text and captures; captures are numbered in this order.
    pub fn parts(&self) -> &[PatternPart<'a>] {
        &self.parts
    }

    pub fn has_captures(&self) -> bool {
        self.parts
            .iter()
            .any(|p| matches!(p, PatternPart::Capture { .. }))
    }
}

/// The topic a pattern route matched, borrowed for one ingest call.
#[derive(Debug, Clone, Copy)]
pub struct TopicMatch<'a> {
    topic: &'a str,
    names: &'a [Box<str>],
    spans: &'a Spans,
    key: Option<KeyId>,
}

impl<'a> TopicMatch<'a> {
    pub(crate) fn new(
        topic: &'a str,
        names: &'a [Box<str>],
        spans: &'a Spans,
        key: Option<KeyId>,
    ) -> Self {
        Self {
            topic,
            names,
            spans,
            key,
        }
    }

    pub fn topic(&self) -> &'a str {
        self.topic
    }

    /// The value of capture `name`.
    pub fn get(&self, name: &str) -> Option<&'a str> {
        let i = self.names.iter().position(|n| &**n == name)?;
        let &(start, end) = self.spans.get(i)?;
        self.topic.get(usize::from(start)..usize::from(end))
    }

    /// `Some` iff the link is keyed.
    pub fn key(&self) -> Option<KeyId> {
        self.key
    }
}

/// A connector's wildcard rules.
pub trait TopicGrammar: Send + Sync {
    fn compile(&self, pattern: &TopicPattern<'_>) -> Result<Box<dyn TopicFilter>, String>;

    /// Whether `a` matches every topic `b` matches. May be conservative.
    fn covers(&self, a: &str, b: &str) -> bool {
        a == b
    }
}

/// One compiled pattern.
pub trait TopicFilter: Send + Sync {
    /// What to subscribe (`sensors/+/temp`).
    fn filter(&self) -> &str;

    /// Matches only [`filter`](Self::filter) itself.
    fn is_literal(&self) -> bool;

    /// Writes capture `i` to `spans[i]`. Topics never exceed `u16::MAX` bytes.
    fn matches(&self, topic: &str, spans: &mut Spans) -> bool;
}

/// No wildcards: string equality, captures rejected.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExactGrammar;

impl TopicGrammar for ExactGrammar {
    fn compile(&self, pattern: &TopicPattern<'_>) -> Result<Box<dyn TopicFilter>, String> {
        if pattern.has_captures() {
            return Err(format!(
                "'{}': this connector does not support topic patterns",
                pattern.as_str()
            ));
        }
        Ok(Box::new(ExactFilter(pattern.as_str().into())))
    }
}

struct ExactFilter(Box<str>);

impl TopicFilter for ExactFilter {
    fn filter(&self) -> &str {
        &self.0
    }

    fn is_literal(&self) -> bool {
        true
    }

    fn matches(&self, topic: &str, _spans: &mut Spans) -> bool {
        topic == &*self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    fn parts(topic: &str) -> Vec<PatternPart<'_>> {
        TopicPattern::parse(topic).unwrap().parts().to_vec()
    }

    #[test]
    fn parse_splits_text_and_captures() {
        use PatternPart::{Capture, Text};
        assert_eq!(
            parts("a/{x}/b/{rest..}"),
            [
                Text("a/"),
                Capture {
                    name: "x",
                    multi: false
                },
                Text("/b/"),
                Capture {
                    name: "rest",
                    multi: true
                },
            ]
        );
        assert_eq!(
            parts("{x}"),
            [Capture {
                name: "x",
                multi: false
            }]
        );
        assert_eq!(parts("a/+/#"), [Text("a/+/#")]);
        assert!(!TopicPattern::parse("a/+/#").unwrap().has_captures());
        assert!(TopicPattern::parse("dev-{id}").unwrap().has_captures());
    }

    #[test]
    fn parse_errors() {
        for (topic, needle) in [
            ("a/{x", "unbalanced '{'"),
            ("a/x}", "unbalanced '}'"),
            ("a/{{x}}", "unbalanced '{'"),
            ("a/{}", "empty capture name"),
            ("a/{..}", "empty capture name"),
            ("a/{x-y}", "may only contain"),
            ("a/{x}/{x..}", "appears twice"),
            ("{a}/{b}/{c}/{d}/{e}/{f}/{g}/{h}/{i}", "more than 8"),
        ] {
            let err = TopicPattern::parse(topic).unwrap_err().to_string();
            assert!(err.contains(needle), "'{topic}': {err}");
        }
        assert!(TopicPattern::parse("{a}/{b}/{c}/{d}/{e}/{f}/{g}/{h}").is_ok());
    }

    #[test]
    fn exact_grammar_compares_strings() {
        let filter = ExactGrammar
            .compile(&TopicPattern::parse("a/+/#").unwrap())
            .unwrap();
        let mut spans = [(0, 0); MAX_CAPTURES];
        assert!(filter.is_literal());
        assert_eq!(filter.filter(), "a/+/#");
        assert!(filter.matches("a/+/#", &mut spans));
        assert!(!filter.matches("a/b/c", &mut spans));
    }

    #[test]
    fn exact_grammar_rejects_captures() {
        let err = ExactGrammar
            .compile(&TopicPattern::parse("a/{x}").unwrap())
            .err()
            .unwrap();
        assert!(err.contains("does not support topic patterns"), "{err}");
    }

    #[test]
    fn default_covers_is_equality() {
        assert!(ExactGrammar.covers("a/b", "a/b"));
        assert!(!ExactGrammar.covers("a/+", "a/b"));
    }
}
