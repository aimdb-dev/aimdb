//! One `MqttConnector` over two protocol backends.
//!
//! Unlike the other connectors, MQTT does not converge on a single protocol
//! implementation. `rumqttc` owns its socket, TLS and reconnect — its
//! `Transport` is a closed enum, so no stream can be injected — while
//! `mountain-mqtt` is generic over `embedded-io-async`. The two stay separate,
//! and this type is the seam between them: a backend can be swapped or removed
//! without touching the other.
//!
//! | Backend | Client | QoS | TLS |
//! |---|---|---|---|
//! | `Native` (feature `tokio-runtime`) | `rumqttc` (std) | 0–2 | rustls |
//! | `Embedded` (feature `embassy-runtime`) | `mountain-mqtt` (`no_std`) | 0–1 | `embedded-tls` |

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;

use aimdb_core::connector::ConnectorBuilder;
use aimdb_core::{AimDb, DbResult};

/// The `rumqttc` backend: a host client owning its own socket and TLS.
#[cfg(feature = "tokio-runtime")]
pub struct Native(crate::tokio_client::MqttConnectorBuilder);

/// The `mountain-mqtt` backend: `no_std`, over a caller-supplied transport.
#[cfg(feature = "embassy-runtime")]
pub struct Embedded<D = crate::embassy_client::NoTransport>(
    crate::embassy_client::MqttConnectorBuilder<D>,
);

/// An MQTT connector over the backend `B`.
pub struct MqttConnector<B> {
    backend: B,
}

#[cfg(feature = "tokio-runtime")]
impl MqttConnector<Native> {
    /// Connect to `broker_url` (`mqtt://host:port` or `mqtts://host:port`).
    ///
    /// Without [`with_client_id`](Self::with_client_id) a random UUID-based
    /// client id is generated at build.
    pub fn new(broker_url: impl Into<alloc::string::String>) -> Self {
        Self {
            backend: Native(crate::tokio_client::MqttConnectorBuilder::new(broker_url)),
        }
    }

    /// Set the MQTT client id.
    pub fn with_client_id(self, client_id: impl Into<alloc::string::String>) -> Self {
        Self {
            backend: Native(self.backend.0.with_client_id(client_id)),
        }
    }
}

#[cfg(feature = "embassy-runtime")]
impl MqttConnector<Embedded> {
    /// Connect to `broker_url`, then supply the transport with
    /// [`transport`](Self::transport) (`mqtt://`) or [`tls`](Self::tls)
    /// (`mqtts://`, feature `embassy-tls`).
    pub fn new(broker_url: impl Into<alloc::string::String>) -> Self {
        Self {
            backend: Embedded(crate::embassy_client::MqttConnectorBuilder::new(broker_url)),
        }
    }

    /// Dial plain sessions through an adapter's stream dialer — the same call
    /// on any runtime's adapter, with no change in this crate.
    pub fn transport<D>(self, dialer: D) -> MqttConnector<Embedded<D>> {
        MqttConnector {
            backend: Embedded(self.backend.0.transport(dialer)),
        }
    }

    /// Provide the network stack and TLS materials for an `mqtts://` broker.
    #[cfg(feature = "embassy-tls")]
    pub fn tls(
        self,
        stack: &'static embassy_net::Stack<'static>,
        options: crate::embassy_tls::TlsOptions,
    ) -> Self {
        Self {
            backend: Embedded(self.backend.0.tls(stack, options)),
        }
    }
}

#[cfg(feature = "embassy-runtime")]
impl<D> MqttConnector<Embedded<D>> {
    /// Set the MQTT client id (defaults to `aimdb-client`).
    pub fn with_client_id(self, client_id: impl Into<alloc::string::String>) -> Self {
        Self {
            backend: Embedded(self.backend.0.with_client_id(client_id)),
        }
    }

    /// Set the broker username and password.
    pub fn with_credentials(
        self,
        username: impl Into<alloc::string::String>,
        password: impl Into<alloc::string::String>,
    ) -> Self {
        Self {
            backend: Embedded(self.backend.0.with_credentials(username, password)),
        }
    }
}

#[cfg(feature = "tokio-runtime")]
impl ConnectorBuilder for MqttConnector<Native> {
    fn build<'a>(
        &'a self,
        db: &'a AimDb,
    ) -> Pin<
        Box<
            dyn Future<Output = DbResult<Vec<Pin<Box<dyn Future<Output = ()> + Send>>>>>
                + Send
                + 'a,
        >,
    > {
        self.backend.0.build(db)
    }

    fn scheme(&self) -> &str {
        self.backend.0.scheme()
    }
}

#[cfg(feature = "embassy-runtime")]
impl<D> ConnectorBuilder for MqttConnector<Embedded<D>>
where
    D: aimdb_core::session::StreamDialer + Clone + Send + Sync + 'static,
    D::Stream: embedded_io_async::Read + embedded_io_async::Write + embedded_io_async::ReadReady,
{
    fn build<'a>(
        &'a self,
        db: &'a AimDb,
    ) -> Pin<
        Box<
            dyn Future<Output = DbResult<Vec<Pin<Box<dyn Future<Output = ()> + Send>>>>>
                + Send
                + 'a,
        >,
    > {
        self.backend.0.build(db)
    }

    fn scheme(&self) -> &str {
        self.backend.0.scheme()
    }
}
