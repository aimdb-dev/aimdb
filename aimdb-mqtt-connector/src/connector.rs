//! One `MqttConnector` over two protocol backends.
//!
//! The two backends cannot converge: `rumqttc`'s `Transport` is a closed enum,
//! so no stream can be injected, while `mountain-mqtt` is generic over
//! `embedded-io-async`. Broker URL, client id and credentials live here rather
//! than in either backend, so there is one set of setters whichever runs.
//!
//! | Backend | Client | QoS | TLS |
//! |---|---|---|---|
//! | `Native` (no transport supplied) | `rumqttc` (std) | 0–2 | rustls |
//! | `Embedded` (`.transport(..)`) | `mountain-mqtt` (`no_std`) | 0–1 | `embedded-tls` |

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use core::time::Duration;

use aimdb_core::connector::ConnectorBuilder;
use aimdb_core::{AimDb, DbResult};

/// Keep-alive used when the caller names none.
pub(crate) const KEEP_ALIVE_DEFAULT_SECS: u16 = 60;

/// The shortest keep-alive accepted. Below this the derived ping interval stops
/// being a meaningful fraction, and the round-trip timeout would outlive the
/// window it is supposed to fit inside.
const KEEP_ALIVE_MIN_SECS: u16 = 10;

/// The runner's collected future type.
type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
/// What [`ConnectorBuilder::build`] returns.
type BuildFuture<'a> = Pin<Box<dyn Future<Output = DbResult<Vec<BoxFuture>>> + Send + 'a>>;

/// The `rumqttc` backend: it owns its socket, TLS and reconnect, so there is
/// nothing here to configure. Selected by supplying no transport.
#[derive(Clone, Copy, Default)]
pub struct Native;

/// The `mountain-mqtt` backend over a caller-supplied transport.
#[cfg(feature = "embedded")]
pub struct Embedded<D> {
    pub(crate) dialer: D,
}

/// The `mountain-mqtt` backend over `embedded-tls`, on the same
/// caller-supplied transport as the plain path.
#[cfg(feature = "embedded-tls")]
pub struct EmbeddedTls<D> {
    pub(crate) dialer: D,
    pub(crate) options: crate::embedded::TlsSlot,
}

/// An MQTT connector over the backend `B`.
pub struct MqttConnector<B = Native> {
    pub(crate) broker_url: String,
    pub(crate) client_id: Option<String>,
    pub(crate) credentials: Option<(String, String)>,
    pub(crate) keep_alive: Duration,
    pub(crate) backend: B,
}

impl MqttConnector<Native> {
    /// Connect to `broker_url` (`mqtt://host:port` or `mqtts://host:port`).
    ///
    /// Without a transport this is the `rumqttc` backend; without
    /// [`with_client_id`](Self::with_client_id) the client id is a generated
    /// UUID.
    pub fn new(broker_url: impl Into<String>) -> Self {
        Self {
            broker_url: broker_url.into(),
            client_id: None,
            credentials: None,
            keep_alive: Duration::from_secs(KEEP_ALIVE_DEFAULT_SECS as u64),
            backend: Native,
        }
    }

    /// Dial plain sessions through an adapter's stream dialer.
    #[cfg(feature = "embedded")]
    pub fn transport<D>(self, dialer: D) -> MqttConnector<Embedded<D>> {
        MqttConnector {
            broker_url: self.broker_url,
            client_id: self.client_id,
            credentials: self.credentials,
            keep_alive: self.keep_alive,
            backend: Embedded { dialer },
        }
    }

    /// Dial `mqtts://` sessions through an adapter's stream dialer, with
    /// `options` supplying the trust root, buffers and entropy.
    #[cfg(feature = "embedded-tls")]
    pub fn tls<D>(
        self,
        dialer: D,
        options: crate::embedded::tls::TlsOptions,
    ) -> MqttConnector<EmbeddedTls<D>> {
        MqttConnector {
            broker_url: self.broker_url,
            client_id: self.client_id,
            credentials: self.credentials,
            keep_alive: self.keep_alive,
            backend: EmbeddedTls {
                dialer,
                options: crate::embedded::TlsSlot::new(options),
            },
        }
    }
}

impl<B> MqttConnector<B> {
    /// Set the MQTT client id (should be unique per device).
    pub fn with_client_id(mut self, client_id: impl Into<String>) -> Self {
        self.client_id = Some(client_id.into());
        self
    }

    /// Authenticate with the broker (MQTT CONNECT username/password).
    ///
    /// Over `mqtt://` the credential transits in cleartext — pair it with
    /// `mqtts://` outside a trusted LAN.
    pub fn with_credentials(
        mut self,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        self.credentials = Some((username.into(), password.into()));
        self
    }

    /// Promise the broker it will hear from this client at least this often
    /// (MQTT CONNECT keep-alive). Defaults to 60 s.
    pub fn with_keep_alive(mut self, keep_alive: Duration) -> Self {
        self.keep_alive = keep_alive;
        self
    }
}

/// Whole seconds for the wire, or the reason this keep-alive cannot be used.
fn keep_alive_secs(keep_alive: Duration) -> DbResult<u16> {
    let secs = keep_alive.as_secs();
    if secs < u64::from(KEEP_ALIVE_MIN_SECS) {
        return Err(aimdb_core::DbError::runtime_error(alloc::format!(
            "MQTT keep-alive must be at least {KEEP_ALIVE_MIN_SECS}s, got {secs}s"
        )));
    }
    u16::try_from(secs).map_err(|_| {
        aimdb_core::DbError::runtime_error(alloc::format!(
            "MQTT keep-alive must fit in u16 seconds (max {}), got {secs}s",
            u16::MAX
        ))
    })
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::Native {}
    #[cfg(feature = "embedded")]
    impl<D> Sealed for super::Embedded<D> {}
    #[cfg(feature = "embedded-tls")]
    impl<D> Sealed for super::EmbeddedTls<D> {}
}

/// A backend with a build path compiled in.
///
/// Implemented for [`Native`] only under `std`, so a `no_std` build that
/// forgets `.transport(..)` fails here rather than deep in core.
#[diagnostic::on_unimplemented(
    message = "`MqttConnector<{Self}>` has no MQTT backend compiled in",
    label = "no backend for this configuration",
    note = "supply a transport — `.transport(dialer)` — for the mountain-mqtt backend, or enable this crate's `std` feature for the rumqttc one"
)]
pub trait Backend: sealed::Sealed + Send + Sync {
    /// Connect and collect this backend's data-plane futures.
    fn build<'a>(
        &'a self,
        db: &'a AimDb,
        broker_url: &'a str,
        client_id: Option<&'a str>,
        credentials: Option<&'a (String, String)>,
        keep_alive_secs: u16,
    ) -> BuildFuture<'a>;
}

#[cfg(feature = "std")]
impl Backend for Native {
    fn build<'a>(
        &'a self,
        db: &'a AimDb,
        broker_url: &'a str,
        client_id: Option<&'a str>,
        credentials: Option<&'a (String, String)>,
        keep_alive_secs: u16,
    ) -> BuildFuture<'a> {
        crate::native::build(db, broker_url, client_id, credentials, keep_alive_secs)
    }
}

#[cfg(feature = "embedded")]
impl<D> Backend for Embedded<D>
where
    D: aimdb_core::session::StreamDialer
        + aimdb_core::session::Delay
        + Clone
        + Send
        + Sync
        + 'static,
{
    fn build<'a>(
        &'a self,
        db: &'a AimDb,
        broker_url: &'a str,
        client_id: Option<&'a str>,
        credentials: Option<&'a (String, String)>,
        keep_alive_secs: u16,
    ) -> BuildFuture<'a> {
        crate::embedded::build_plain(
            db,
            broker_url,
            client_id,
            credentials,
            keep_alive_secs,
            &self.dialer,
        )
    }
}

#[cfg(feature = "embedded-tls")]
impl<D> Backend for EmbeddedTls<D>
where
    D: aimdb_core::session::StreamDialer
        + aimdb_core::session::Delay
        + Clone
        + Send
        + Sync
        + 'static,
{
    fn build<'a>(
        &'a self,
        db: &'a AimDb,
        broker_url: &'a str,
        client_id: Option<&'a str>,
        credentials: Option<&'a (String, String)>,
        keep_alive_secs: u16,
    ) -> BuildFuture<'a> {
        crate::embedded::build_tls(
            db,
            broker_url,
            client_id,
            credentials,
            keep_alive_secs,
            self,
        )
    }
}

impl<B: Backend> ConnectorBuilder for MqttConnector<B> {
    fn build<'a>(&'a self, db: &'a AimDb) -> BuildFuture<'a> {
        // Checked here rather than in either backend: one keep-alive, one
        // rejection, whichever one runs.
        let keep_alive_secs = match keep_alive_secs(self.keep_alive) {
            Ok(secs) => secs,
            Err(e) => return Box::pin(async move { Err(e) }),
        };
        self.backend.build(
            db,
            &self.broker_url,
            self.client_id.as_deref(),
            self.credentials.as_ref(),
            keep_alive_secs,
        )
    }

    fn scheme(&self) -> &str {
        "mqtt"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_keep_alive_is_whole_seconds_on_the_wire() {
        assert_eq!(keep_alive_secs(Duration::from_secs(60)).unwrap(), 60);
        // Truncated, not rounded: the promise must never overstate the gap.
        assert_eq!(
            keep_alive_secs(Duration::from_millis(60_900)).unwrap(),
            60,
            "a partial second must round down, so the broker is never told to \
             wait longer than we actually allow"
        );
    }

    #[test]
    fn a_keep_alive_that_cannot_be_honoured_is_refused() {
        // Zero means "no keep-alive" in MQTT; the derived cadence has no
        // meaning there, so it is refused rather than reinterpreted.
        assert!(keep_alive_secs(Duration::ZERO).is_err());
        assert!(keep_alive_secs(Duration::from_secs(9)).is_err());
        assert!(keep_alive_secs(Duration::from_secs(u64::from(KEEP_ALIVE_MIN_SECS) - 1)).is_err());
        // The wire field is u16 seconds.
        assert!(keep_alive_secs(Duration::from_secs(u64::from(u16::MAX) + 1)).is_err());
        assert_eq!(
            keep_alive_secs(Duration::from_secs(u64::from(u16::MAX))).unwrap(),
            u16::MAX
        );
    }
}
