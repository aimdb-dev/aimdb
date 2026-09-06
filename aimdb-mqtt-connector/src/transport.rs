//! The broker transport seam for the [`Embedded`](crate::connector::Embedded)
//! backend.
//!
//! Built on `mountain-mqtt`'s own [`Connection`] rather than core's
//! [`ByteStream`](aimdb_core::session::ByteStream): the MQTT client needs
//! `receive_if_ready` — a non-blocking peek — which a byte stream does not
//! express and a TLS session cannot provide (its readiness is two-layered;
//! see [`crate::embassy_tls`]). Wrapping core's trait would mean every
//! TLS-like transport faking a capability, so the client's own seam is the
//! honest one.
//!
//! A new runtime supplies MQTT by implementing this once. Anything offering
//! `embedded_io_async::{Read, Write}` plus `ReadReady` — an lwIP socket, say —
//! gets there through `mountain_mqtt::embedded_io_async::ConnectionEmbedded`
//! with no protocol code to touch.

use aimdb_core::session::TransportResult;
use core::future::Future;
use mountain_mqtt::packet_client::Connection;

/// Opens one broker connection per session.
///
/// The connector calls this once per reconnect cycle, so an implementation
/// must be able to produce a fresh connection each time.
pub trait BrokerTransport {
    /// The connection this transport produces.
    type Connection: Connection;

    /// Open a connection to the broker.
    fn connect(&self) -> impl Future<Output = TransportResult<Self::Connection>> + Send;
}

/// Bridges core's [`StreamDialer`](aimdb_core::session::StreamDialer) to
/// [`BrokerTransport`] for any adapter whose stream also offers the
/// `embedded-io-async` trio.
///
/// This is the path a new runtime takes: implement `StreamDialer` and delegate
/// `Read`/`Write`/`ReadReady` on the stream, and MQTT follows with no code
/// here. TLS does not come this way — its readiness is two-layered, so it
/// implements [`BrokerTransport`] directly.
pub struct SocketTransport<D> {
    dialer: D,
    host: alloc::string::String,
    port: u16,
}

impl<D> SocketTransport<D> {
    /// Dial `host:port` through `dialer` for each broker session.
    pub fn new(dialer: D, host: impl Into<alloc::string::String>, port: u16) -> Self {
        Self {
            dialer,
            host: host.into(),
            port,
        }
    }
}

impl<D> BrokerTransport for SocketTransport<D>
where
    D: aimdb_core::session::StreamDialer + Sync,
    D::Stream: embedded_io_async::Read + embedded_io_async::Write + embedded_io_async::ReadReady,
{
    type Connection = mountain_mqtt::embedded_io_async::ConnectionEmbedded<D::Stream>;

    async fn connect(&self) -> TransportResult<Self::Connection> {
        let stream = self.dialer.connect(&self.host, self.port).await?;
        Ok(mountain_mqtt::embedded_io_async::ConnectionEmbedded::new(
            stream,
        ))
    }
}
