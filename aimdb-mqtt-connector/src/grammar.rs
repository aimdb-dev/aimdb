//! MQTT topic filters as a [`TopicGrammar`] (MQTT 3.1.1 §4.7).

use aimdb_core::{PatternPart, Spans, TopicFilter, TopicGrammar, TopicPattern};
use alloc::{boxed::Box, format, string::String, vec::Vec};

/// MQTT 3.1.1 §4.7: `/` separates levels, `+` matches one level, `#` the rest
/// including the parent level (`a/#` matches `a`) and only as the last level.
/// A wildcard is a whole level, and a leading wildcard does not match a `$…`
/// topic.
#[derive(Debug, Clone, Copy, Default)]
pub struct MqttGrammar;

enum Level {
    Literal(String),
    /// `+` or `{name}`, with the capture number.
    Single(Option<usize>),
    /// `#` or `{name..}`, with the capture number.
    Multi(Option<usize>),
}

struct MqttFilter {
    filter: String,
    levels: Vec<Level>,
}

/// A level as written: its text and, for `{…}`, the capture number and
/// whether it is `{name..}`.
#[derive(Default)]
struct RawLevel {
    text: String,
    capture: Option<(usize, bool)>,
}

impl TopicGrammar for MqttGrammar {
    fn compile(&self, pattern: &TopicPattern<'_>) -> Result<Box<dyn TopicFilter>, String> {
        let topic = pattern.as_str();

        let mut raw = Vec::from([RawLevel::default()]);
        let mut captures = 0;
        for part in pattern.parts() {
            match part {
                PatternPart::Text(text) => {
                    for (i, segment) in text.split('/').enumerate() {
                        if i > 0 {
                            raw.push(RawLevel::default());
                        }
                        if let Some(level) = raw.last_mut() {
                            level.text.push_str(segment);
                        }
                    }
                }
                PatternPart::Capture { multi, .. } => {
                    if let Some(level) = raw.last_mut() {
                        level.capture = Some((captures, *multi));
                    }
                    captures += 1;
                }
            }
        }

        let last = raw.len() - 1;
        let mut levels = Vec::with_capacity(raw.len());
        for (i, RawLevel { text, capture }) in raw.into_iter().enumerate() {
            let level = match (capture, text.as_str()) {
                (Some(_), t) if !t.is_empty() => {
                    return Err(format!(
                        "'{topic}': a capture must be a whole level, not part of one"
                    ))
                }
                (Some((n, false)), _) => Level::Single(Some(n)),
                (Some((n, true)), _) => Level::Multi(Some(n)),
                (None, "+") => Level::Single(None),
                (None, "#") => Level::Multi(None),
                (None, t) if t.contains(['+', '#']) => {
                    return Err(format!(
                        "'{topic}': a wildcard must be a whole level, not part of '{t}'"
                    ))
                }
                (None, _) => Level::Literal(text),
            };
            if matches!(level, Level::Multi(_)) && i != last {
                return Err(format!(
                    "'{topic}': a multi-level wildcard must be the last level"
                ));
            }
            levels.push(level);
        }

        let filter = levels
            .iter()
            .map(|level| match level {
                Level::Literal(text) => text.as_str(),
                Level::Single(_) => "+",
                Level::Multi(_) => "#",
            })
            .collect::<Vec<_>>()
            .join("/");
        Ok(Box::new(MqttFilter { filter, levels }))
    }

    fn covers(&self, a: &str, b: &str) -> bool {
        let (a, b): (Vec<&str>, Vec<&str>) = (a.split('/').collect(), b.split('/').collect());
        for i in 0.. {
            match (a.get(i).copied(), b.get(i).copied()) {
                // The rest of `b`, possibly nothing; at level 0 not a `$…` topic.
                (Some("#"), first) => {
                    return i > 0 || !first.is_some_and(|l| l.starts_with('$'));
                }
                (Some("+"), Some(level)) if level != "#" => {
                    if i == 0 && level.starts_with('$') {
                        return false;
                    }
                }
                (Some(x), Some(y)) if x == y && x != "+" => {}
                (None, None) => return true,
                _ => return false,
            }
        }
        false
    }
}

impl TopicFilter for MqttFilter {
    fn filter(&self) -> &str {
        &self.filter
    }

    fn is_literal(&self) -> bool {
        self.levels.iter().all(|l| matches!(l, Level::Literal(_)))
    }

    fn matches(&self, topic: &str, spans: &mut Spans) -> bool {
        // The router never passes more than `u16::MAX` bytes.
        let span = |start: usize, end: usize| (start as u16, end as u16);
        let mut rest = Some(topic);
        let mut pos = 0;

        for (i, level) in self.levels.iter().enumerate() {
            if let Level::Multi(capture) = level {
                if i == 0 && topic.starts_with('$') {
                    return false;
                }
                if let Some(n) = capture {
                    let start = if rest.is_some() { pos } else { topic.len() };
                    spans[*n] = span(start, topic.len());
                }
                return true;
            }

            let Some(r) = rest else { return false };
            let (text, next) = match r.split_once('/') {
                Some((text, next)) => (text, Some(next)),
                None => (r, None),
            };
            match level {
                Level::Literal(lit) if lit != text => return false,
                Level::Single(capture) => {
                    if i == 0 && text.starts_with('$') {
                        return false;
                    }
                    if let Some(n) = capture {
                        spans[*n] = span(pos, pos + text.len());
                    }
                }
                _ => {}
            }
            pos += text.len() + 1;
            rest = next;
        }
        rest.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aimdb_core::MAX_CAPTURES;
    use alloc::string::ToString;

    fn compile(topic: &str) -> Result<Box<dyn TopicFilter>, String> {
        MqttGrammar.compile(&TopicPattern::parse(topic).map_err(|e| e.to_string())?)
    }

    fn is_match(pattern: &str, topic: &str) -> bool {
        compile(pattern)
            .unwrap()
            .matches(topic, &mut [(0, 0); MAX_CAPTURES])
    }

    /// Named captures of `topic`, or `None` when it does not match.
    fn captures(pattern: &str, topic: &str) -> Option<Vec<(String, String)>> {
        let parsed = TopicPattern::parse(pattern).unwrap();
        let names = parsed.parts().iter().filter_map(|p| match p {
            PatternPart::Capture { name, .. } => Some(name.to_string()),
            PatternPart::Text(_) => None,
        });
        let mut spans = [(0, 0); MAX_CAPTURES];
        let filter = MqttGrammar.compile(&parsed).unwrap();
        filter.matches(topic, &mut spans).then(|| {
            names
                .zip(spans)
                .map(|(n, (s, e))| (n, topic[usize::from(s)..usize::from(e)].to_string()))
                .collect()
        })
    }

    fn caps(pairs: &[(&str, &str)]) -> Option<Vec<(String, String)>> {
        Some(
            pairs
                .iter()
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn filter_renders_captures_as_wildcards() {
        assert_eq!(
            compile("sensors/{d}/temp").unwrap().filter(),
            "sensors/+/temp"
        );
        assert_eq!(compile("a/{rest..}").unwrap().filter(), "a/#");
        assert_eq!(compile("a/+/{x}/#").unwrap().filter(), "a/+/+/#");
        assert!(compile("a/b").unwrap().is_literal());
        assert!(!compile("a/+").unwrap().is_literal());
    }

    #[test]
    fn compile_errors() {
        for (topic, needle) in [
            ("sensors/dev-{id}", "capture must be a whole level"),
            ("a/{x}y", "capture must be a whole level"),
            ("a/{rest..}/b", "must be the last level"),
            ("a/#/b", "must be the last level"),
            ("sport/tennis#", "wildcard must be a whole level"),
            ("sport+", "wildcard must be a whole level"),
        ] {
            let err = compile(topic).err().unwrap();
            assert!(err.contains(needle), "'{topic}': {err}");
        }
    }

    // §4.7.1.2
    #[test]
    fn multi_level_wildcard() {
        let p = "sport/tennis/player1/#";
        assert!(is_match(p, "sport/tennis/player1"));
        assert!(is_match(p, "sport/tennis/player1/ranking"));
        assert!(is_match(p, "sport/tennis/player1/score/wimbledon"));
        assert!(!is_match(p, "sport/tennis/player2"));
        assert!(is_match("sport/#", "sport"));
        assert!(is_match("#", "anything/at/all"));
    }

    // §4.7.1.3
    #[test]
    fn single_level_wildcard() {
        assert!(is_match("sport/tennis/+", "sport/tennis/player1"));
        assert!(!is_match("sport/tennis/+", "sport/tennis/player1/ranking"));
        assert!(!is_match("sport/+", "sport"));
        assert!(is_match("sport/+", "sport/"));
        assert!(is_match("+/+", "/finance"));
        assert!(is_match("/+", "/finance"));
        assert!(!is_match("+", "/finance"));
        assert!(is_match("+/tennis/#", "sport/tennis/x"));
    }

    // §4.7.2
    #[test]
    fn dollar_topics_are_hidden_from_leading_wildcards() {
        assert!(!is_match("#", "$SYS/monitor/Clients"));
        assert!(!is_match("+/monitor/Clients", "$SYS/monitor/Clients"));
        assert!(is_match("$SYS/#", "$SYS/monitor/Clients"));
        assert!(is_match("$SYS/monitor/+", "$SYS/monitor/Clients"));
        assert!(is_match("a/+", "a/$x"));
    }

    #[test]
    fn captures_at_first_middle_and_last_level() {
        assert_eq!(
            captures("{d}/temp", "kitchen/temp"),
            caps(&[("d", "kitchen")])
        );
        assert_eq!(
            captures("sensors/{d}/temp", "sensors/kitchen/temp"),
            caps(&[("d", "kitchen")])
        );
        assert_eq!(
            captures("sensors/{d}", "sensors/kitchen"),
            caps(&[("d", "kitchen")])
        );
        assert_eq!(
            captures("{site}/+/{d}", "vienna/x/k1"),
            caps(&[("site", "vienna"), ("d", "k1")])
        );
        assert_eq!(captures("sensors/{d}/temp", "sensors/kitchen/hum"), None);
    }

    #[test]
    fn rest_capture_takes_the_remainder_or_nothing() {
        assert_eq!(
            captures("logs/{rest..}", "logs/a/b/c"),
            caps(&[("rest", "a/b/c")])
        );
        assert_eq!(captures("logs/{rest..}", "logs"), caps(&[("rest", "")]));
        assert_eq!(captures("logs/{rest..}", "logs/"), caps(&[("rest", "")]));
        assert_eq!(captures("logs/{rest..}", "other"), None);
        assert_eq!(captures("a/{x}/b", "a//b"), caps(&[("x", "")]));
    }

    #[test]
    fn covering() {
        let g = MqttGrammar;
        assert!(g.covers("sensors/+/temp", "sensors/kitchen/temp"));
        assert!(!g.covers("sensors/kitchen/temp", "sensors/+/temp"));
        assert!(g.covers("a/#", "a"));
        assert!(g.covers("a/#", "a/+/b"));
        assert!(g.covers("a/#", "a/#"));
        assert!(g.covers("a/+", "a/+"));
        assert!(!g.covers("a/+", "a/#"));
        assert!(!g.covers("a/+", "b/+"));
        assert!(!g.covers("a/+", "a/+/c"));
        assert!(!g.covers("a/+/c", "a/#"));
        assert!(!g.covers("a/+", "a"));
    }

    #[test]
    fn covering_respects_dollar_topics() {
        let g = MqttGrammar;
        assert!(!g.covers("#", "$SYS/x"));
        assert!(!g.covers("+/x", "$SYS/x"));
        assert!(g.covers("$SYS/#", "$SYS/x"));
        assert!(g.covers("#", "+/x"));
        assert!(g.covers("sensors/+", "sensors/$x"));
    }
}
