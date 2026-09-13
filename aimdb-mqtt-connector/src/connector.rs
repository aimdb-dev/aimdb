//! One `MqttConnector` over two protocol backends.
//!
//! Unlike the other connectors, MQTT does not converge on a single protocol
//! implementation. `rumqttc` owns its socket, TLS and reconnect — its
//! `Transport` is a closed enum, so no stream can be injected — while
//! `mountain-mqtt` is generic over `embedded-io-async`. The two stay separate,
//! and this type is the seam between them.
//!
//! Broker URL, client id and credentials live here rather than in a backend, so
//! there is one constructor and one set of setters whichever backend runs.
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

use aimdb_core::connector::ConnectorBuilder;
use aimdb_core::{AimDb, DbResult};

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
    pub(crate) backend: B,
}

impl MqttConnector<Native> {
    /// Connect to `broker_url` (`mqtt://host:port` or `mqtts://host:port`).
    ///
    /// Without a transport this is the `rumqttc` backend, and without
    /// [`with_client_id`](Self::with_client_id) it generates a UUID-based
    /// client id at build.
    pub fn new(broker_url: impl Into<String>) -> Self {
        Self {
            broker_url: broker_url.into(),
            client_id: None,
            credentials: None,
            backend: Native,
        }
    }

    /// Dial plain sessions through an adapter's stream dialer — the same call
    /// on any runtime's adapter, with no change in this crate.
    #[cfg(feature = "embedded")]
    pub fn transport<D>(self, dialer: D) -> MqttConnector<Embedded<D>> {
        MqttConnector {
            broker_url: self.broker_url,
            client_id: self.client_id,
            credentials: self.credentials,
            backend: Embedded { dialer },
        }
    }

    /// Dial `mqtts://` sessions through an adapter's stream dialer, with
    /// `options` supplying the trust root, buffers and entropy.
    ///
    /// The dialer resolves the host, so TLS needs no network stack of its own.
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
/// Implemented for [`Native`] only under `std`, so a `no_std` build
/// that forgets `.transport(..)` fails here with a message naming the fix
/// rather than on core's `ConnectorBuilder`.
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
    ) -> BuildFuture<'a> {
        crate::native::build(db, broker_url, client_id, credentials)
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
    D::Stream: embedded_io_async::Read + embedded_io_async::Write + embedded_io_async::ReadReady,
{
    fn build<'a>(
        &'a self,
        db: &'a AimDb,
        broker_url: &'a str,
        client_id: Option<&'a str>,
        credentials: Option<&'a (String, String)>,
    ) -> BuildFuture<'a> {
        crate::embedded::build_plain(db, broker_url, client_id, credentials, &self.dialer)
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
    D::Stream: embedded_io_async::Read + embedded_io_async::Write + embedded_io_async::ReadReady,
{
    fn build<'a>(
        &'a self,
        db: &'a AimDb,
        broker_url: &'a str,
        client_id: Option<&'a str>,
        credentials: Option<&'a (String, String)>,
    ) -> BuildFuture<'a> {
        crate::embedded::build_tls(db, broker_url, client_id, credentials, self)
    }
}

impl<B: Backend> ConnectorBuilder for MqttConnector<B> {
    fn build<'a>(&'a self, db: &'a AimDb) -> BuildFuture<'a> {
        self.backend.build(
            db,
            &self.broker_url,
            self.client_id.as_deref(),
            self.credentials.as_ref(),
        )
    }

    fn scheme(&self) -> &str {
        "mqtt"
    }
}
