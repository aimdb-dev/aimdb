//! `host:port` endpoint grammar, shared by every transport that speaks it.
//!
//! It lives here rather than in a connector because more than one crate needs
//! it and they do not all depend on each other: `aimdb-client` resolves
//! `scheme://` URLs whether or not the matching connector is compiled in.
//! One grammar, one implementation, one set of tests.
//!
//! Callers layer their own policy on top: [`split_host_port_opt`] reports
//! whether a port was written at all, so one caller can default it while
//! another rejects an endpoint that omits it.

use alloc::string::{String, ToString};

/// Why an endpoint is not a `host:port`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointError {
    /// The endpoint names no host.
    EmptyHost,
    /// A bracketed IPv6 literal with no closing `]`.
    UnclosedBracket,
    /// A port was written, and is not one.
    BadPort,
}

impl core::fmt::Display for EndpointError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::EmptyHost => "endpoint names no host",
            Self::UnclosedBracket => "missing closing bracket for IPv6 host",
            Self::BadPort => "port is not a number in 0..=65535",
        })
    }
}

/// Split a `host:port` endpoint, leaving the port `None` when none was written.
///
/// A bracketed IPv6 literal carries colons of its own, so only a colon *after*
/// the closing bracket separates the port; the brackets are stripped, because
/// that is the form the adapters resolve. An **unbracketed** IPv6 literal
/// cannot carry a port at all — brackets are what add one — so every colon in
/// it belongs to the address.
///
/// A port that is written but is not one is an error, never a silent fallback:
/// dialing a different service is worse than not dialing.
pub fn split_host_port_opt(endpoint: &str) -> Result<(String, Option<u16>), EndpointError> {
    if let Some(rest) = endpoint.strip_prefix('[') {
        let Some((host, tail)) = rest.split_once(']') else {
            return Err(EndpointError::UnclosedBracket);
        };
        if host.is_empty() {
            return Err(EndpointError::EmptyHost);
        }
        let port = if tail.is_empty() {
            None
        } else {
            // Anything but `:port` after `]` is junk, including junk with a port
            // behind it — `[::1]oops:7003` names no reachable service.
            Some(
                tail.strip_prefix(':')
                    .ok_or(EndpointError::BadPort)?
                    .parse()
                    .map_err(|_| EndpointError::BadPort)?,
            )
        };
        return Ok((host.to_string(), port));
    }
    match endpoint.rsplit_once(':') {
        // Another colon before the last one: an unbracketed IPv6 literal, whose
        // trailing group is part of the address rather than a port.
        Some((head, _)) if head.contains(':') => Ok((endpoint.to_string(), None)),
        Some(("", _)) => Err(EndpointError::EmptyHost),
        Some((host, port)) => Ok((
            host.to_string(),
            Some(port.parse().map_err(|_| EndpointError::BadPort)?),
        )),
        None if endpoint.is_empty() => Err(EndpointError::EmptyHost),
        None => Ok((endpoint.to_string(), None)),
    }
}

/// As [`split_host_port_opt`], substituting `default_port` when the endpoint
/// names only a host.
pub fn split_host_port(endpoint: &str, default_port: u16) -> Result<(String, u16), EndpointError> {
    let (host, port) = split_host_port_opt(endpoint)?;
    Ok((host, port.unwrap_or(default_port)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT: u16 = 7001;

    fn split(endpoint: &str) -> Result<(String, u16), EndpointError> {
        split_host_port(endpoint, DEFAULT)
    }

    #[test]
    fn splits_host_and_port() {
        assert_eq!(split("127.0.0.1:7002"), Ok(("127.0.0.1".into(), 7002)));
    }

    #[test]
    fn a_bare_host_takes_the_default_port() {
        assert_eq!(split("example.test"), Ok(("example.test".into(), DEFAULT)));
        assert_eq!(
            split_host_port_opt("example.test"),
            Ok(("example.test".into(), None)),
            "the caller can tell a defaulted port from a written one"
        );
    }

    /// A written port that is not one names no service, so it is an error
    /// rather than a silent fallback to the default — that would dial a
    /// different, possibly live, server.
    #[test]
    fn an_unparsable_port_is_rejected() {
        assert_eq!(split("host:not-a-port"), Err(EndpointError::BadPort));
        assert_eq!(split("10.0.0.5:8080x"), Err(EndpointError::BadPort));
        assert_eq!(split("host:99999"), Err(EndpointError::BadPort));
        assert_eq!(split("host:"), Err(EndpointError::BadPort));
    }

    /// A bracketed IPv6 literal is full of colons; only the one after `]`
    /// separates the port, and the brackets are not part of the address.
    #[test]
    fn brackets_are_stripped_from_an_ipv6_literal() {
        assert_eq!(split("[::1]:7003"), Ok(("::1".into(), 7003)));
    }

    #[test]
    fn a_bracketed_ipv6_host_without_a_port_is_not_mangled() {
        assert_eq!(split("[::1]"), Ok(("::1".into(), DEFAULT)));
    }

    /// Brackets are what let an IPv6 literal carry a port, so without them
    /// every colon belongs to the address and no port was written.
    #[test]
    fn an_unbracketed_ipv6_literal_keeps_all_its_colons() {
        assert_eq!(split("::1"), Ok(("::1".into(), DEFAULT)));
        assert_eq!(split("fe80::1"), Ok(("fe80::1".into(), DEFAULT)));
        assert_eq!(
            split("2001:db8::dead:beef"),
            Ok(("2001:db8::dead:beef".into(), DEFAULT)),
            "the trailing group is address, not a port"
        );
    }

    #[test]
    fn a_malformed_endpoint_is_rejected() {
        assert_eq!(split(":99999"), Err(EndpointError::EmptyHost));
        assert_eq!(split(""), Err(EndpointError::EmptyHost));
        assert_eq!(split("[]:7001"), Err(EndpointError::EmptyHost));
        assert_eq!(split("[::1"), Err(EndpointError::UnclosedBracket));
        assert_eq!(
            split("[::1]oops:7003"),
            Err(EndpointError::BadPort),
            "an explicit port behind junk is not silently honoured"
        );
    }
}
