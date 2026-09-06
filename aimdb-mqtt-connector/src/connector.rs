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

/// The `mountain-mqtt` backend: `no_std`, over the device's network stack.
#[cfg(feature = "embassy-runtime")]
pub struct Embedded(crate::embassy_client::MqttConnectorBuilder);

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
    /// Connect to `broker_url` over the device's network stack.
    ///
    /// `mqtt://` is plain TCP (default port 1883); `mqtts://` is TLS
    /// (default 8883) and needs the `embassy-tls` feature plus
    /// `with_tls` (feature `embassy-tls`).
    pub fn new(
        broker_url: impl Into<alloc::string::String>,
        stack: &'static embassy_net::Stack<'static>,
    ) -> Self {
        Self {
            backend: Embedded(crate::embassy_client::MqttConnectorBuilder::new(
                broker_url, stack,
            )),
        }
    }

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

    /// Provide the TLS materials for an `mqtts://` broker.
    #[cfg(feature = "embassy-tls")]
    pub fn with_tls(self, options: crate::embassy_tls::TlsOptions) -> Self {
        Self {
            backend: Embedded(self.backend.0.with_tls(options)),
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
impl ConnectorBuilder for MqttConnector<Embedded> {
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
