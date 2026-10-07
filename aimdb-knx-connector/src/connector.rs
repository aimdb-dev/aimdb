//! Runtime-neutral KNX connector.
//!
//! Generic over core's [`DatagramBinder`](aimdb_core::session::DatagramBinder)
//! and [`Delay`](aimdb_core::session::Delay), so the adapter owns the UDP
//! socket and the clock while this crate owns the tunnelling protocol. The one
//! connection task dispatches inbound telegrams into their records and pulls
//! outbound values from their record buffers itself; core runs no pump for it.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::future::Future;
use core::net::SocketAddr;
use core::pin::Pin;

use aimdb_core::connector::{ConnectorBuilder, ConnectorUrl};
use aimdb_core::{log_info, AimDb, DbError, DbResult, InboundDispatch, OutboundRoutes, RuntimeOps};

use crate::client::connection_task;

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Default KNXnet/IP tunnelling port.
const DEFAULT_PORT: u16 = 3671;

/// KNX/IP tunnelling connector over an adapter's datagram transport.
pub struct KnxConnector<B, D> {
    binder: B,
    delay: D,
    gateway_url: String,
}

impl<B, D> KnxConnector<B, D> {
    /// Connect to the KNX/IP gateway at `gateway_url` (`knx://host:port`).
    ///
    /// `binder` and `delay` come from an adapter.
    pub fn new(binder: B, delay: D, gateway_url: impl Into<String>) -> Self {
        Self {
            binder,
            delay,
            gateway_url: gateway_url.into(),
        }
    }

    /// Parse and validate the gateway address.
    ///
    /// Checked at build so a typo'd IP surfaces as an error rather than a
    /// parked connection task. Hostnames are not resolved.
    fn gateway_addr(&self) -> DbResult<SocketAddr> {
        let url = ConnectorUrl::parse(&self.gateway_url)
            .map_err(|e| DbError::runtime_error(alloc::format!("Invalid KNX URL: {e}")))?;
        let port = url.port.unwrap_or(DEFAULT_PORT);
        alloc::format!("{}:{}", url.host, port)
            .parse()
            .map_err(|_| {
                DbError::runtime_error(alloc::format!(
                    "Invalid KNX gateway address {}:{} (an IP address is required; \
                     hostnames are not resolved)",
                    url.host,
                    port
                ))
            })
    }
}

impl<B, D> ConnectorBuilder for KnxConnector<B, D>
where
    B: aimdb_core::session::DatagramBinder + Clone + Send + Sync + 'static,
    D: aimdb_core::session::Delay + Clone + Send + Sync + 'static,
{
    fn build<'a>(
        &'a self,
        db: &'a AimDb,
    ) -> Pin<Box<dyn Future<Output = DbResult<Vec<BoxFuture>>> + Send + 'a>> {
        Box::pin(async move {
            let gateway = self.gateway_addr()?;
            log_info!("Creating KNX connector for gateway {}", gateway);

            let runtime: Arc<dyn RuntimeOps> = db.runtime_ops();
            // Built here, so routes are subscribed before the task first runs:
            // a command produced while the tunnel connects waits in its record
            // buffer and goes out once the handshake completes.
            let inbound = InboundDispatch::new(db, "knx", &aimdb_core::ExactGrammar)?;
            let outbound = OutboundRoutes::new(db, "knx")?;
            let task: BoxFuture = Box::pin(connection_task(
                self.binder.clone(),
                gateway,
                runtime,
                self.delay.clone(),
                inbound,
                outbound,
            ));
            Ok(vec![task])
        })
    }

    fn scheme(&self) -> &str {
        "knx"
    }

    /// One KNX connector per db.
    ///
    /// Not a limit on group addresses — one connector is one tunnel to one
    /// gateway, and that is the whole bus behind it: every record's
    /// `link_from`/`link_to` names its own address, and they all ride this one
    /// connector. What it rules out is a *second gateway*, which cannot work
    /// today because `build` collects every `knx://` route regardless of which
    /// gateway it was meant for.
    fn owns_scheme(&self) -> bool {
        true
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use aimdb_core::buffer::BufferCfg;
    use aimdb_core::AimDbBuilder;
    use aimdb_tokio_adapter::net::{TokioDelay, TokioNet};
    use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
    use std::net::Ipv4Addr;

    async fn db() -> AimDb {
        let mut builder = AimDbBuilder::new().runtime(Arc::new(TokioAdapter));
        builder.configure::<u64>("light", |reg| {
            reg.buffer(BufferCfg::SingleLatest).with_remote_access();
        });
        builder.build().await.expect("build db").0
    }

    /// A typo'd gateway must fail at `build`, not park a connection task.
    #[tokio::test]
    async fn an_unparsable_gateway_fails_the_build() {
        let db = db().await;
        let connector = KnxConnector::new(
            TokioNet::udp(Ipv4Addr::LOCALHOST),
            TokioDelay,
            "knx://not-an-ip:3671",
        );
        let Err(err) = connector.build(&db).await else {
            panic!("a hostname must be rejected: it is never resolved");
        };
        assert!(
            format!("{err}").contains("an IP address is required"),
            "unexpected error: {err}"
        );
    }

    /// A db with one inbound and one outbound `knx` route.
    ///
    /// A connector must be registered or the builder rejects the routes ("no
    /// connector registered for scheme 'knx'"). Nothing here is ever driven —
    /// `build` only collects futures.
    async fn routed_db() -> AimDb {
        let mut builder = AimDbBuilder::new()
            .runtime(Arc::new(TokioAdapter))
            .with_connector(KnxConnector::new(
                TokioNet::udp(Ipv4Addr::LOCALHOST),
                TokioDelay,
                "knx://127.0.0.1:3671",
            ));
        builder.configure::<u64>("switch", |reg| {
            reg.buffer(BufferCfg::SingleLatest)
                .link_from("knx://1/0/7")
                .with_deserializer(
                    |_ctx, data: &[u8]| Ok(data.first().copied().unwrap_or(0) as u64),
                )
                .finish();
        });
        builder.configure::<u64>("lamp", |reg| {
            reg.buffer(BufferCfg::SingleLatest)
                .link_to("knx://1/0/6")
                .with_serializer(|_ctx, v: &u64| Ok(vec![*v as u8]))
                .finish();
        });
        builder.build().await.expect("build db").0
    }

    /// The connector registers under the `knx` scheme and contributes exactly
    /// one future, the connection task, which dispatches inbound telegrams and
    /// pulls outbound values itself.
    #[tokio::test]
    async fn build_yields_only_the_connection_task() {
        let connector = || {
            KnxConnector::new(
                TokioNet::udp(Ipv4Addr::LOCALHOST),
                TokioDelay,
                "knx://127.0.0.1:3671",
            )
        };
        assert_eq!(ConnectorBuilder::scheme(&connector()), "knx");

        // With inbound and outbound routes, and with none.
        for db in [routed_db().await, db().await] {
            let futures = connector().build(&db).await.expect("build");
            assert_eq!(futures.len(), 1, "the connection task alone");
        }
    }

    /// A second KNX connector is a configuration error: each connector
    /// collects *all* `knx://` routes, so every route would be served twice
    /// and nothing says which gateway it belongs to.
    #[tokio::test]
    async fn a_second_knx_connector_fails_the_build() {
        let builder = AimDbBuilder::new()
            .runtime(Arc::new(TokioAdapter))
            .with_connector(KnxConnector::new(
                TokioNet::udp(Ipv4Addr::LOCALHOST),
                TokioDelay,
                "knx://127.0.0.1:3671",
            ))
            .with_connector(KnxConnector::new(
                TokioNet::udp(Ipv4Addr::LOCALHOST),
                TokioDelay,
                "knx://127.0.0.2:3671",
            ));

        let Err(err) = builder.build().await else {
            panic!("a second KNX connector must be rejected");
        };
        assert!(
            format!("{err}").contains("More than one connector registered for scheme 'knx'"),
            "unexpected error: {err}"
        );
    }

    /// The whole wiring against a real UDP gateway: the task binds, advertises
    /// its endpoint, and the handshake reaches the wire.
    ///
    /// Binds `LOCALHOST`, not the `UNSPECIFIED` the demos and `aimdb-codegen`
    /// pass, precisely so `local_addr()` yields a routable address and the
    /// explicit-HPAI branch is the one under test. The NAT branch that an
    /// unspecified bind takes has its own test in `client`.
    #[tokio::test]
    async fn the_wired_connector_reaches_a_gateway() {
        let gateway = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("bind gateway");
        let addr = gateway.local_addr().expect("gateway addr");

        let db = db().await;
        let connector = KnxConnector::new(
            TokioNet::udp(Ipv4Addr::LOCALHOST),
            TokioDelay,
            format!("knx://{addr}"),
        );
        let futures = connector.build(&db).await.expect("build");
        let driving: Vec<_> = futures.into_iter().map(tokio::spawn).collect();

        let mut buf = [0u8; 128];
        let (len, _) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            gateway.recv_from(&mut buf),
        )
        .await
        .expect("no CONNECT_REQUEST reached the gateway")
        .expect("recv_from");

        assert!(len >= 14, "CONNECT_REQUEST carries both HPAIs");
        assert_eq!(
            u16::from_be_bytes([buf[2], buf[3]]),
            0x0205,
            "CONNECT_REQUEST"
        );
        assert_ne!(
            &buf[8..12],
            &[0, 0, 0, 0],
            "the real local endpoint is advertised"
        );

        for handle in driving {
            handle.abort();
        }
    }
}
