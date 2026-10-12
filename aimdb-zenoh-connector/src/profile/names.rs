//! ROS 2 name rules, as `rmw`'s validators state them.

use core::fmt;

/// The longest fully qualified topic name (`RMW_TOPIC_MAX_NAME_LENGTH`).
const TOPIC_MAX: usize = 255 - 8;
/// The longest namespace (`RMW_NAMESPACE_MAX_LENGTH`).
const NAMESPACE_MAX: usize = TOPIC_MAX - 2;
/// The longest node name (`RMW_NODE_NAME_MAX_NAME_LENGTH`).
const NODE_NAME_MAX: usize = 255;

/// Why a topic, namespace or node name breaks the ROS rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NameError {
    Empty,
    NotAbsolute,
    EndsWithSlash,
    Characters,
    RepeatedSlash,
    StartsWithDigit,
    TooLong { max: usize },
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("is empty"),
            Self::NotAbsolute => f.write_str("must start with '/'"),
            Self::EndsWithSlash => f.write_str("must not end with '/'"),
            Self::Characters => f.write_str("may only hold letters, digits, '_' and '/'"),
            Self::RepeatedSlash => f.write_str("must not hold '//'"),
            Self::StartsWithDigit => f.write_str("has a name part starting with a digit"),
            Self::TooLong { max } => write!(f, "is longer than {max} bytes"),
        }
    }
}

/// A fully qualified topic name: `/cell4/temperature`.
pub(crate) fn validate_topic(name: &str) -> Result<(), NameError> {
    absolute(name, TOPIC_MAX)
}

/// A namespace: `/` or `/cell4`.
pub(crate) fn validate_namespace(name: &str) -> Result<(), NameError> {
    if name == "/" {
        return Ok(());
    }
    absolute(name, NAMESPACE_MAX)
}

/// A node name: `cell4_gateway`.
pub(crate) fn validate_node_name(name: &str) -> Result<(), NameError> {
    let Some(first) = name.bytes().next() else {
        return Err(NameError::Empty);
    };
    if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return Err(NameError::Characters);
    }
    if first.is_ascii_digit() {
        return Err(NameError::StartsWithDigit);
    }
    if name.len() > NODE_NAME_MAX {
        return Err(NameError::TooLong { max: NODE_NAME_MAX });
    }
    Ok(())
}

fn absolute(name: &str, max: usize) -> Result<(), NameError> {
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if !name.starts_with('/') {
        return Err(NameError::NotAbsolute);
    }
    if name.ends_with('/') {
        return Err(NameError::EndsWithSlash);
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'/')
    {
        return Err(NameError::Characters);
    }
    if name.contains("//") {
        return Err(NameError::RepeatedSlash);
    }
    if name[1..]
        .split('/')
        .any(|part| part.bytes().next().is_some_and(|b| b.is_ascii_digit()))
    {
        return Err(NameError::StartsWithDigit);
    }
    if name.len() > max {
        return Err(NameError::TooLong { max });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topics_follow_rmw() {
        for good in ["/chatter", "/cell4/temperature", "/_private", "/a1/b_2"] {
            assert_eq!(validate_topic(good), Ok(()), "{good}");
        }
        for (bad, why) in [
            ("", NameError::Empty),
            ("chatter", NameError::NotAbsolute),
            ("/chatter/", NameError::EndsWithSlash),
            ("/", NameError::EndsWithSlash),
            ("/cell-4", NameError::Characters),
            ("/~/x", NameError::Characters),
            ("/{node}/x", NameError::Characters),
            ("/a//b", NameError::RepeatedSlash),
            ("/4cell/x", NameError::StartsWithDigit),
            ("/cell/4x", NameError::StartsWithDigit),
        ] {
            assert_eq!(validate_topic(bad), Err(why), "{bad}");
        }
        let long = alloc::format!("/{}", "a".repeat(TOPIC_MAX));
        assert_eq!(validate_topic(&long[..TOPIC_MAX]), Ok(()));
        assert_eq!(validate_topic(&long), Err(NameError::TooLong { max: 247 }));
    }

    #[test]
    fn namespaces_allow_the_root() {
        assert_eq!(validate_namespace("/"), Ok(()));
        assert_eq!(validate_namespace("/cell4"), Ok(()));
        assert_eq!(validate_namespace("/cell4/"), Err(NameError::EndsWithSlash));
        assert_eq!(validate_namespace("cell4"), Err(NameError::NotAbsolute));
        let long = alloc::format!("/{}", "a".repeat(NAMESPACE_MAX));
        assert_eq!(
            validate_namespace(&long),
            Err(NameError::TooLong { max: 245 })
        );
    }

    #[test]
    fn node_names_are_one_token() {
        assert_eq!(validate_node_name("cell4_gateway"), Ok(()));
        assert_eq!(validate_node_name(""), Err(NameError::Empty));
        assert_eq!(validate_node_name("cell/4"), Err(NameError::Characters));
        assert_eq!(validate_node_name("cell-4"), Err(NameError::Characters));
        assert_eq!(validate_node_name("4cell"), Err(NameError::StartsWithDigit));
        let long = "a".repeat(NODE_NAME_MAX + 1);
        assert_eq!(
            validate_node_name(&long),
            Err(NameError::TooLong { max: 255 })
        );
    }
}
