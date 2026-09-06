//! Runtime-neutral KNX connector.
//!
//! Generic over core's [`DatagramBinder`](aimdb_core::session::DatagramBinder)
//! and [`Delay`](aimdb_core::session::Delay), so the adapter owns the UDP
//! socket and the clock while this crate owns the tunnelling protocol.
//! The channels between the pumps and the connection task are `embassy_sync`,
//! which is executor-independent, so one wiring serves both runtimes.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::future::Future;
use core::net::SocketAddr;
use core::pin::Pin;

use aimdb_core::connector::{ConnectorBuilder, ConnectorUrl};
use aimdb_core::session::{pump_sink, pump_source, Payload};
use aimdb_core::transport::{Connector, ConnectorConfig, PublishError};
use aimdb_core::{log_info, AimDb, DbError, DbResult, RuntimeOps};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;

use crate::client::{connection_task, shared_channel::ChannelCommands, TelegramSink};
use crate::tunnel::GroupWrite;

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Default KNXnet/IP tunnelling port.
const DEFAULT_PORT: u16 = 3671;

/// Capacity of the command and telegram channels.
///
/// A const generic rather than a builder setter: the MCU allocates these in a
/// `static`, where the size must be a constant.
pub const DEFAULT_QUEUE: usize = 32;

/// The inbound-telegram channel type.
pub type TelegramChannel<const N: usize> = Channel<CriticalSectionRawMutex, (String, Payload), N>;
/// The outbound-command channel type.
pub type CommandChannel<const N: usize> = Channel<CriticalSectionRawMutex, GroupWrite, N>;

/// Outbound half: `pump_sink` hands each serialized record here.
struct KnxSink<'a, const N: usize> {
    commands: &'a CommandChannel<N>,
}

impl<const N: usize> Connector for KnxSink<'_, N> {
    fn publish(
        &self,
        destination: &str,
        _config: &ConnectorConfig,
        payload: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), PublishError>> + Send + '_>> {
        // Validation shared with the connection task (same checks, same order).
        let command = GroupWrite::try_new(destination, payload);
        Box::pin(async move {
            self.commands.send(command?).await;
            Ok(())
        })
    }
}

/// Inbound half: the connection task pushes telegrams here for `pump_source`.
struct ChannelTelegrams<'a, const N: usize>(&'a TelegramChannel<N>);

impl<const N: usize> TelegramSink for ChannelTelegrams<'_, N> {
    fn try_send(&self, topic: String, payload: Payload) -> bool {
        self.0.try_send((topic, payload)).is_ok()
    }
}

/// Inbound source drained by `pump_source`.
struct KnxSource<'a, const N: usize> {
    telegrams: &'a TelegramChannel<N>,
}

impl<const N: usize> aimdb_core::session::Source for KnxSource<'_, N> {
    fn next(&mut self) -> aimdb_core::session::BoxFut<'_, Option<(String, Payload)>> {
        Box::pin(async move { Some(self.telegrams.receive().await) })
    }
}

/// KNX/IP tunnelling connector over an adapter's datagram transport.
///
/// `N` sizes both the command and telegram channels.
pub struct KnxConnector<B, D, const N: usize = DEFAULT_QUEUE> {
    binder: B,
    delay: D,
    gateway_url: String,
    channels: &'static Channels<N>,
}

/// The channel pair, held for the process lifetime.
///
/// `'static` because the connection task and the pumps are spawned as
/// `'static` futures; a `StaticCell` supplies this on the MCU and a leak at
/// build does on a host, matching design 037's allocate-at-build model.
pub struct Channels<const N: usize> {
    telegrams: TelegramChannel<N>,
    commands: CommandChannel<N>,
}

impl<const N: usize> Default for Channels<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Channels<N> {
    /// A fresh, empty channel pair.
    pub const fn new() -> Self {
        Self {
            telegrams: Channel::new(),
            commands: Channel::new(),
        }
    }
}

impl<B, D, const N: usize> KnxConnector<B, D, N> {
    /// Connect to the KNX/IP gateway at `gateway_url` (`knx://host:port`).
    ///
    /// `binder` and `delay` come from an adapter; `channels` is the caller's
    /// `'static` channel pair.
    pub fn new(
        binder: B,
        delay: D,
        gateway_url: impl Into<String>,
        channels: &'static Channels<N>,
    ) -> Self {
        Self {
            binder,
            delay,
            gateway_url: gateway_url.into(),
            channels,
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

/// Host constructor: the Tokio transports and a leaked channel pair, so a
/// caller (and `aimdb-codegen`) needs only the gateway URL.
///
/// The leak is one allocation at build for the process lifetime — the channels
/// must outlive the `'static` task and pump futures. An MCU uses
/// [`KnxConnector::new`] with a `StaticCell` instead.
#[cfg(feature = "tokio-runtime")]
impl
    KnxConnector<
        aimdb_tokio_adapter::net::TokioUdpBinder,
        aimdb_tokio_adapter::net::TokioDelay,
        DEFAULT_QUEUE,
    >
{
    /// Connect to the KNX/IP gateway at `gateway_url` (`knx://host:port`).
    pub fn tokio(gateway_url: impl Into<String>) -> Self {
        use core::net::Ipv4Addr;
        Self::new(
            aimdb_tokio_adapter::net::TokioNet::udp(Ipv4Addr::UNSPECIFIED),
            aimdb_tokio_adapter::net::TokioDelay,
            gateway_url,
            Box::leak(Box::new(Channels::new())),
        )
    }
}

impl<B, D, const N: usize> ConnectorBuilder for KnxConnector<B, D, N>
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
            let channels = self.channels;
            let task: BoxFuture = Box::pin(connection_task(
                self.binder.clone(),
                gateway,
                runtime,
                self.delay.clone(),
                ChannelTelegrams::<N>(&channels.telegrams),
                ChannelCommands::<N>(channels.commands.receiver()),
            ));

            let mut futures: Vec<BoxFuture> = vec![task];
            futures.extend(pump_source(
                db,
                "knx",
                KnxSource::<N> {
                    telegrams: &channels.telegrams,
                },
            ));
            futures.extend(pump_sink(
                db,
                "knx",
                Arc::new(KnxSink::<N> {
                    commands: &channels.commands,
                }),
            ));
            Ok(futures)
        })
    }

    fn scheme(&self) -> &str {
        "knx"
    }
}

#[cfg(all(test, feature = "tokio-runtime"))]
mod tests {
    use super::*;
    use aimdb_core::buffer::BufferCfg;
    use aimdb_core::AimDbBuilder;
    use aimdb_tokio_adapter::net::{TokioDelay, TokioNet};
    use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
    use std::net::Ipv4Addr;

    static CHANNELS: Channels<8> = Channels::new();

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
        let connector = KnxConnector::<_, _, 8>::new(
            TokioNet::udp(Ipv4Addr::LOCALHOST),
            TokioDelay,
            "knx://not-an-ip:3671",
            &CHANNELS,
        );
        let Err(err) = connector.build(&db).await else {
            panic!("a hostname must be rejected: it is never resolved");
        };
        assert!(
            format!("{err}").contains("an IP address is required"),
            "unexpected error: {err}"
        );
    }

    /// The connector registers under the `knx` scheme and contributes the
    /// connection task plus its pump futures.
    #[tokio::test]
    async fn build_yields_the_connection_task_and_pumps() {
        static CH: Channels<8> = Channels::new();
        let db = db().await;
        let connector = KnxConnector::<_, _, 8>::new(
            TokioNet::udp(Ipv4Addr::LOCALHOST),
            TokioDelay,
            "knx://127.0.0.1:3671",
            &CH,
        );
        assert_eq!(ConnectorBuilder::scheme(&connector), "knx");

        let futures = connector.build(&db).await.expect("build");
        assert!(
            !futures.is_empty(),
            "at least the connection task is contributed"
        );
    }

    /// The whole wiring against a real UDP gateway: the task binds, advertises
    /// its endpoint, and the handshake reaches the wire.
    #[tokio::test]
    async fn the_wired_connector_reaches_a_gateway() {
        static CH: Channels<8> = Channels::new();
        let gateway = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("bind gateway");
        let addr = gateway.local_addr().expect("gateway addr");

        let db = db().await;
        let connector = KnxConnector::<_, _, 8>::new(
            TokioNet::udp(Ipv4Addr::LOCALHOST),
            TokioDelay,
            format!("knx://{addr}"),
            &CH,
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
