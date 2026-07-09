//! Endpoint resolution — map a `scheme://` URL (or a bare path) to a transport
//! [`Dialer`], so an operator can pick the transport at runtime the same way
//! records pick one for links.
//!
//! Two layers, deliberately split so the grammar is testable without any
//! transport compiled in:
//! - [`parse_endpoint`] — **pure, feature-independent**. Recognizes the scheme
//!   grammar (`unix://` / `uds://` / `serial://` / `tcp://host:port`, plus a
//!   bare path as `unix://` shorthand) into a [`ParsedEndpoint`]. An unknown
//!   scheme is rejected here.
//! - [`dial`] — builds the concrete [`Dialer`] for a parsed endpoint, under the
//!   matching `transport-*` feature. A scheme whose transport isn't compiled into
//!   this binary is rejected here (distinct from "unknown scheme").
//!
use aimdb_core::session::Dialer;

use crate::error::{ClientError, ClientResult};

/// Default serial baud when a `serial://` endpoint omits `?baud=`.
pub const DEFAULT_SERIAL_BAUD: u32 = 115_200;

/// Schemes the resolver's *grammar* understands, independent of which transports
/// are compiled in. Used to phrase the "unknown scheme" error.
const KNOWN_SCHEMES: &[&str] = &["unix", "uds", "serial", "tcp"];

/// The transport family an endpoint names. Always compiled (it is grammar, not a
/// capability) — whether a given variant can actually be dialed depends on the
/// `transport-*` features, enforced in [`dial`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    /// A Unix-domain socket (`unix://` / `uds://`, or a bare path).
    Unix,
    /// A serial/UART device (`serial://`).
    Serial,
    /// A TCP endpoint (`tcp://host:port`).
    Tcp,
}

/// A parsed endpoint: the transport family plus its target and any transport
/// options (currently just the serial baud).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedEndpoint {
    /// Which transport family this endpoint names.
    pub scheme: Scheme,
    /// The transport target — a socket path, serial device path, or host:port.
    pub target: String,
    /// Serial baud from `?baud=N`, if given (`Serial` only; [`dial`] defaults it
    /// to [`DEFAULT_SERIAL_BAUD`]).
    pub baud: Option<u32>,
}

/// Parse an endpoint string into a [`ParsedEndpoint`] (pure; no transport needed).
///
/// - `unix://PATH` / `uds://PATH` → [`Scheme::Unix`].
/// - a bare path (no `scheme://`) → [`Scheme::Unix`] (the shorthand).
/// - `serial://PATH` (optionally `?baud=N`) → [`Scheme::Serial`].
/// - `tcp://HOST:PORT` → [`Scheme::Tcp`].
/// - anything else → [`ClientError::UnsupportedEndpoint`].
pub fn parse_endpoint(endpoint: &str) -> ClientResult<ParsedEndpoint> {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        return Err(ClientError::unsupported_endpoint("", "empty endpoint"));
    }

    // No `scheme://` → a bare path is the `unix://` shorthand.
    let Some((scheme, rest)) = endpoint.split_once("://") else {
        return Ok(ParsedEndpoint {
            scheme: Scheme::Unix,
            target: endpoint.to_string(),
            baud: None,
        });
    };

    match scheme.to_ascii_lowercase().as_str() {
        "unix" | "uds" => {
            require_nonempty(endpoint, rest)?;
            Ok(ParsedEndpoint {
                scheme: Scheme::Unix,
                target: rest.to_string(),
                baud: None,
            })
        }
        "serial" => {
            // Split off an optional `?baud=N[&…]` query; unknown query keys are
            // ignored for forward-compat, but a malformed `baud` is an error.
            let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
            require_nonempty(endpoint, path)?;
            let baud = parse_baud(endpoint, query)?;
            Ok(ParsedEndpoint {
                scheme: Scheme::Serial,
                target: path.to_string(),
                baud,
            })
        }
        "tcp" => {
            require_tcp_target(endpoint, rest)?;
            Ok(ParsedEndpoint {
                scheme: Scheme::Tcp,
                target: rest.to_string(),
                baud: None,
            })
        }
        other => Err(ClientError::unsupported_endpoint(
            endpoint,
            format!(
                "unknown scheme {other:?}; built-in schemes: {}",
                KNOWN_SCHEMES.join(", ")
            ),
        )),
    }
}

/// Resolve an endpoint string to a boxed [`Dialer`] for the linked-in transport.
///
/// Errors with [`ClientError::UnsupportedEndpoint`] if the string is malformed,
/// names an unknown scheme, or names a scheme whose transport isn't compiled into
/// this binary.
pub fn dial(endpoint: &str) -> ClientResult<Box<dyn Dialer>> {
    let parsed = parse_endpoint(endpoint)?;
    match parsed.scheme {
        Scheme::Unix => {
            #[cfg(feature = "transport-uds")]
            {
                Ok(Box::new(aimdb_uds_connector::UdsDialer::new(parsed.target)))
            }
            #[cfg(not(feature = "transport-uds"))]
            {
                Err(not_built_in(endpoint, "unix", "transport-uds"))
            }
        }
        Scheme::Serial => {
            #[cfg(feature = "transport-serial")]
            {
                Ok(Box::new(aimdb_serial_connector::SerialDialer::new(
                    parsed.target,
                    parsed.baud.unwrap_or(DEFAULT_SERIAL_BAUD),
                )))
            }
            #[cfg(not(feature = "transport-serial"))]
            {
                Err(not_built_in(endpoint, "serial", "transport-serial"))
            }
        }
        Scheme::Tcp => {
            #[cfg(feature = "transport-tcp")]
            {
                Ok(Box::new(aimdb_tcp_connector::TcpDialer::new(parsed.target)))
            }
            #[cfg(not(feature = "transport-tcp"))]
            {
                Err(not_built_in(endpoint, "tcp", "transport-tcp"))
            }
        }
    }
}

/// Reject an empty target (e.g. `unix://`), pointing at the original endpoint.
fn require_nonempty(endpoint: &str, target: &str) -> ClientResult<()> {
    if target.is_empty() {
        Err(ClientError::unsupported_endpoint(
            endpoint,
            "missing path after scheme",
        ))
    } else {
        Ok(())
    }
}

/// Validate `tcp://host:port`.
fn require_tcp_target(endpoint: &str, target: &str) -> ClientResult<()> {
    require_nonempty(endpoint, target)?;

    let (host, port) = if let Some(rest) = target.strip_prefix('[') {
        let Some((host, after_host)) = rest.split_once(']') else {
            return Err(ClientError::unsupported_endpoint(
                endpoint,
                "missing closing bracket for IPv6 TCP host",
            ));
        };
        let Some(port) = after_host.strip_prefix(':') else {
            return Err(ClientError::unsupported_endpoint(
                endpoint,
                "missing TCP port",
            ));
        };
        (host, port)
    } else {
        if target.matches(':').count() > 1 {
            return Err(ClientError::unsupported_endpoint(
                endpoint,
                "IPv6 TCP hosts must be bracketed, e.g. tcp://[::1]:7001",
            ));
        }
        let Some((host, port)) = target.split_once(':') else {
            return Err(ClientError::unsupported_endpoint(
                endpoint,
                "missing TCP port",
            ));
        };
        if host.contains(['[', ']']) {
            return Err(ClientError::unsupported_endpoint(
                endpoint,
                "malformed TCP host",
            ));
        }
        (host, port)
    };

    if host.is_empty() {
        return Err(ClientError::unsupported_endpoint(
            endpoint,
            "missing TCP host",
        ));
    }
    if port.is_empty() {
        return Err(ClientError::unsupported_endpoint(
            endpoint,
            "missing TCP port",
        ));
    }
    port.parse::<u16>().map_err(|_| {
        ClientError::unsupported_endpoint(endpoint, format!("invalid TCP port {port:?}"))
    })?;
    Ok(())
}

/// Pull `baud` out of a `serial://` query string (`baud=N[&k=v…]`).
fn parse_baud(endpoint: &str, query: &str) -> ClientResult<Option<u32>> {
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key.eq_ignore_ascii_case("baud") {
            let baud = value.parse::<u32>().map_err(|_| {
                ClientError::unsupported_endpoint(endpoint, format!("invalid baud {value:?}"))
            })?;
            return Ok(Some(baud));
        }
    }
    Ok(None)
}

/// A recognized scheme whose transport feature isn't compiled in.
#[cfg(any(
    not(feature = "transport-uds"),
    not(feature = "transport-serial"),
    not(feature = "transport-tcp")
))]
fn not_built_in(endpoint: &str, scheme: &str, feature: &str) -> ClientError {
    ClientError::unsupported_endpoint(
        endpoint,
        format!(
            "scheme {scheme:?} is not built into this binary (rebuild with --features {feature})"
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_scheme_and_bare_path_both_resolve_to_unix() {
        for ep in ["unix:///tmp/aimdb.sock", "uds:///tmp/aimdb.sock"] {
            let p = parse_endpoint(ep).expect("parse");
            assert_eq!(p.scheme, Scheme::Unix);
            assert_eq!(p.target, "/tmp/aimdb.sock");
            assert_eq!(p.baud, None);
        }
        // Bare paths (absolute + relative) are the `unix://` shorthand.
        for bare in ["/tmp/aimdb.sock", "./rel.sock"] {
            let p = parse_endpoint(bare).expect("parse");
            assert_eq!(p.scheme, Scheme::Unix);
            assert_eq!(p.target, bare);
        }
    }

    #[test]
    fn serial_scheme_parses_path_and_optional_baud() {
        let p = parse_endpoint("serial:///dev/ttyACM0?baud=9600").expect("parse");
        assert_eq!(p.scheme, Scheme::Serial);
        assert_eq!(p.target, "/dev/ttyACM0");
        assert_eq!(p.baud, Some(9600));

        // No query → baud unset (dial defaults it to DEFAULT_SERIAL_BAUD).
        let p = parse_endpoint("serial:///dev/ttyUSB0").expect("parse");
        assert_eq!(p.baud, None);

        // Unknown query keys are ignored; baud is still picked up.
        let p = parse_endpoint("serial:///dev/ttyUSB0?foo=bar&baud=230400").expect("parse");
        assert_eq!(p.baud, Some(230400));
    }

    #[test]
    fn tcp_scheme_parses_host_and_port() {
        let p = parse_endpoint("tcp://127.0.0.1:7001").expect("parse");
        assert_eq!(p.scheme, Scheme::Tcp);
        assert_eq!(p.target, "127.0.0.1:7001");

        let p = parse_endpoint("tcp://localhost:7001").expect("parse");
        assert_eq!(p.scheme, Scheme::Tcp);
        assert_eq!(p.target, "localhost:7001");

        let p = parse_endpoint("tcp://[::1]:7001").expect("parse");
        assert_eq!(p.scheme, Scheme::Tcp);
        assert_eq!(p.target, "[::1]:7001");

        let p = parse_endpoint("tcp://[fe80::1]:7001").expect("parse");
        assert_eq!(p.scheme, Scheme::Tcp);
        assert_eq!(p.target, "[fe80::1]:7001");
    }

    #[test]
    fn malformed_endpoints_are_rejected() {
        // Malformed TCP.
        assert!(parse_endpoint("tcp://").is_err());
        assert!(parse_endpoint("tcp://host").is_err());
        assert!(parse_endpoint("tcp://:1234").is_err());
        assert!(parse_endpoint("tcp://host:fast").is_err());
        assert!(parse_endpoint("tcp://fe80::1").is_err());
        assert!(parse_endpoint("tcp://[fe80::1]").is_err());
        assert!(parse_endpoint("tcp://[]:7001").is_err());
        assert!(parse_endpoint("tcp://[fe80::1:7001").is_err());
        // Empty + empty target.
        assert!(parse_endpoint("").is_err());
        assert!(parse_endpoint("unix://").is_err());
        // Non-numeric baud.
        assert!(parse_endpoint("serial:///dev/x?baud=fast").is_err());
    }

    #[test]
    fn dial_rejects_unknown_scheme() {
        assert!(matches!(
            dial("http://host:1234"),
            Err(ClientError::UnsupportedEndpoint { .. })
        ));
    }

    #[cfg(feature = "transport-uds")]
    #[test]
    fn dial_builds_a_unix_dialer() {
        assert!(dial("unix:///tmp/aimdb.sock").is_ok());
        assert!(dial("/tmp/aimdb.sock").is_ok());
    }

    #[cfg(not(feature = "transport-serial"))]
    #[test]
    fn dial_rejects_serial_when_not_built_in() {
        assert!(matches!(
            dial("serial:///dev/ttyACM0"),
            Err(ClientError::UnsupportedEndpoint { .. })
        ));
    }

    #[cfg(feature = "transport-serial")]
    #[test]
    fn dial_builds_a_serial_dialer() {
        assert!(dial("serial:///dev/ttyACM0?baud=115200").is_ok());
    }

    #[cfg(not(feature = "transport-tcp"))]
    #[test]
    fn dial_rejects_tcp_when_not_built_in() {
        assert!(matches!(
            dial("tcp://127.0.0.1:7001"),
            Err(ClientError::UnsupportedEndpoint { .. })
        ));
    }

    #[cfg(feature = "transport-tcp")]
    #[test]
    fn dial_builds_a_tcp_dialer() {
        assert!(dial("tcp://127.0.0.1:7001").is_ok());
    }
}
