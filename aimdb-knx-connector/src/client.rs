//! One KNX connection task, generic over core's [`DatagramBinder`] and
//! [`Delay`], replacing the two hand-written socket loops.
//!
//! The clock stays `RuntimeOps::now_nanos`, a plain call; only *sleeping* goes
//! through [`Delay`], so nothing is boxed per loop iteration.
//!
//! The task dispatches inbound telegrams into their records through an
//! [`InboundDispatch`](aimdb_core::InboundDispatch) and pulls outbound values
//! from an [`OutboundRoutes`] while the tunnel is connected. `embassy-futures`'
//! select is executor-independent, so the task runs on either runtime.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Write as _;
use core::future::poll_fn;
use core::net::SocketAddr;
use core::task::Poll;
use core::time::Duration;

use aimdb_core::connector::TopicBuf;
use aimdb_core::session::{Datagram, DatagramBinder, Delay, TransportError, TransportResult};
use aimdb_core::{log_debug, log_error, log_warn, OutboundRoutes, RouteId, RuntimeOps};

use crate::tunnel::{
    drain_actions, GroupWrite, LocalEndpoint, Millis, TunnelConfig, TunnelEngine, TunnelIo,
};
use crate::GroupAddress;

/// Backoff before retrying a bind that failed.
const BIND_RETRY: Duration = Duration::from_secs(5);
/// Per-datagram receive buffer; a KNXnet/IP frame fits comfortably.
const RECV_BUF: usize = 512;

/// Where parsed telegrams go: an [`InboundDispatch`](aimdb_core::InboundDispatch)
/// in the connector.
///
/// Non-blocking by contract: delivery never stalls the protocol loop.
pub trait TelegramSink {
    /// Deliver one telegram for group address `topic`.
    fn deliver(&self, topic: &str, payload: &[u8]);
}

impl TelegramSink for aimdb_core::InboundDispatch {
    fn deliver(&self, topic: &str, payload: &[u8]) {
        self.dispatch(topic, payload);
    }
}

/// The longest group address, `31/7/255`, is 8 bytes.
const GROUP_ADDRESS_LEN: usize = 16;

/// The socket-side glue for [`drain_actions`], written once against
/// [`Datagram`] instead of once per runtime.
struct NeutralIo<'a, U, S> {
    socket: &'a mut U,
    gateway: SocketAddr,
    sink: &'a S,
}

impl<U, S> TunnelIo for NeutralIo<'_, U, S>
where
    U: Datagram + Send,
    S: TelegramSink + Sync,
{
    // A plain `async fn` satisfies the trait's `+ Send` return bound — the
    // adapter's `Datagram` impl already carries whatever force-`Send` its
    // runtime needs.
    async fn send(&mut self, frame: &[u8]) -> bool {
        // Log-and-continue: a transient send error must not tear down the
        // tunnel; a persistently dead send path surfaces through the engine's
        // heartbeat-response timeout.
        match self.socket.send_to(frame, self.gateway).await {
            Ok(()) => true,
            Err(_) => {
                log_error!("KNX send failed");
                false
            }
        }
    }

    fn forward(&mut self, addr: GroupAddress, payload: Vec<u8>) {
        log_debug!("KNX telegram: {} ({} bytes)", addr, payload.len());
        let mut storage = [0u8; GROUP_ADDRESS_LEN];
        let mut topic = TopicBuf::new(&mut storage);
        if write!(topic, "{addr}").is_ok() {
            self.sink.deliver(topic.as_str(), &payload);
        }
    }

    fn warn_ack_timeout(&mut self, _seq: u8) {
        log_warn!("KNX outbound: no ACK for sequence {}", _seq);
    }
}

/// Hand the value `outbound` staged for route `id` to the engine as a group
/// write. An invalid group address or an oversize payload is logged, counted
/// as rejected in the route's `RouteStats`, and skipped.
fn send_staged(engine: &mut TunnelEngine, outbound: &mut OutboundRoutes, id: RouteId, now: Millis) {
    let command = {
        let Some(msg) = outbound.take_staged() else {
            return;
        };
        GroupWrite::try_new(msg.topic, msg.payload.as_slice())
    };
    match command {
        Ok(command) => {
            let _ = engine.handle_command(command, now);
        }
        Err(_e) => {
            log_warn!(
                "KNX outbound: skipping a value for route '{}': {:?}",
                outbound.routes()[id].default_topic,
                _e
            );
            outbound.reject(id);
        }
    }
}

/// Drive the engine over one socket's lifetime; returns when it asks for a reset.
async fn drive_connection<U, D, S>(
    engine: &mut TunnelEngine,
    socket: &mut U,
    gateway: SocketAddr,
    runtime: &Arc<dyn RuntimeOps>,
    delay: &D,
    sink: &S,
    outbound: &mut OutboundRoutes,
) where
    U: Datagram + Send,
    D: Delay,
    S: TelegramSink + Sync,
{
    // Executor-independent despite the name: `embassy-futures` has no
    // dependencies and its select is pure `core::task`.
    use embassy_futures::select::{select3, Either3};

    /// Apply one received datagram, shared by the two arm orders below.
    ///
    /// A free function rather than a common `Event` enum: `GroupWrite` is large
    /// enough that funnelling both arms through one value would park a second
    /// copy of it in this task's state for the whole loop.
    fn apply_inbound(
        engine: &mut TunnelEngine,
        buf: &[u8],
        result: TransportResult<(usize, SocketAddr)>,
        now: Millis,
    ) {
        match result {
            Ok((len, _peer)) => engine.handle_datagram(&buf[..len], now),
            Err(_) => engine.handle_socket_error(now),
        }
    }

    let now_ms = || runtime.now_nanos() / 1_000_000;

    // `select3` polls its arms in declaration order and takes the first ready
    // one — unlike the `tokio::select!` this task replaces, which picked among
    // the ready arms at random. With a fixed order, sustained inbound traffic
    // means the first arm is ready on every pass and the command arm is never
    // reached, so outbound values stall in their record buffers.
    // Swapping the two contended arms each pass restores that fairness.
    let mut inbound_first = true;
    // Latched on `Ready(None)` (every route closed, or none): passing it
    // through would resolve the arm on every pass and the loop would spin.
    let mut outbound_done = false;

    loop {
        engine.poll(now_ms());

        {
            let mut io = NeutralIo {
                socket,
                gateway,
                sink,
            };
            if drain_actions(engine, &mut io).await {
                return;
            }
        }

        let sleep_ms = engine.next_deadline().saturating_sub(now_ms());
        let deadline = delay.sleep(Duration::from_millis(sleep_ms));
        let mut recv_buf = [0u8; RECV_BUF];

        // Only pull values while connected: during connect and backoff the
        // arm stays pending, so values wait in their record buffers and go out
        // once the handshake completes. A value leaves its buffer only when
        // the arm resolves, so a losing arm takes nothing.
        let connected = engine.is_connected();
        let cmd_arm = poll_fn(|cx| {
            if !connected || outbound_done {
                return Poll::Pending;
            }
            match outbound.poll_stage(cx) {
                Poll::Ready(Some(id)) => Poll::Ready(id),
                Poll::Ready(None) => {
                    outbound_done = true;
                    Poll::Pending
                }
                Poll::Pending => Poll::Pending,
            }
        });

        // The deadline arm stays last in both orders: it only ever asks for a
        // `poll` the loop top would reach anyway.
        if inbound_first {
            match select3(socket.recv_from(&mut recv_buf), cmd_arm, deadline).await {
                Either3::First(r) => apply_inbound(engine, &recv_buf, r, now_ms()),
                Either3::Second(id) => send_staged(engine, outbound, id, now_ms()),
                // Woken for the engine deadline; `poll` at the loop top fires it.
                Either3::Third(()) => {}
            }
        } else {
            match select3(cmd_arm, socket.recv_from(&mut recv_buf), deadline).await {
                Either3::First(id) => send_staged(engine, outbound, id, now_ms()),
                Either3::Second(r) => apply_inbound(engine, &recv_buf, r, now_ms()),
                Either3::Third(()) => {}
            }
        }
        inbound_first = !inbound_first;
    }
}

/// The unified connection task: one body, both runtimes.
///
/// Binds a socket, advertises its real local endpoint when the stack exposes
/// one, drives the shared [`TunnelEngine`] over that socket's lifetime, then
/// rebinds after the engine's backoff.
pub async fn connection_task<B, D, S>(
    binder: B,
    gateway: SocketAddr,
    runtime: Arc<dyn RuntimeOps>,
    delay: D,
    sink: S,
    mut outbound: OutboundRoutes,
) where
    B: DatagramBinder,
    D: Delay,
    S: TelegramSink + Sync,
{
    let now_ms = || runtime.now_nanos() / 1_000_000;
    let mut engine = TunnelEngine::new(TunnelConfig::default(), now_ms());

    loop {
        let mut socket = match binder.bind(0).await {
            Ok(socket) => socket,
            // `Busy` is a caller mistake, not a transient fault: the binder's
            // one socket is held elsewhere — typically a clone of a
            // single-socket binder — and no amount of retrying frees it. Same
            // recovery either way (a `Busy` binder *can* free up if the other
            // holder drops), but the log has to say which, or the misuse reads
            // as an endless unexplained bind failure.
            Err(TransportError::Busy) => {
                log_error!(
                    "KNX bind failed: the binder's socket is held by another handle \
                     (a clone of a single-socket binder?); retrying"
                );
                delay.sleep(BIND_RETRY).await;
                continue;
            }
            Err(_) => {
                log_error!("KNX bind failed; retrying");
                delay.sleep(BIND_RETRY).await;
                continue;
            }
        };

        // The handshake advertises the client's own endpoint (HPAI). Gateways
        // that reject the NAT-style `0.0.0.0:0` form need the real address.
        //
        // `engine` outlives the loop, so this must be set on *every* cycle, not
        // just the ones that can answer. On Embassy `local_addr()` is `None`
        // whenever the stack has no address — DHCP renewal, link flap — which
        // is exactly what causes a rebind. Leaving the previous cycle's value
        // in place would advertise a port nothing is bound to any more and wedge
        // the handshake for good; NAT is degraded but recovers.
        //
        // An unspecified IP is NAT too, not an address. Binding `0.0.0.0` is the
        // normal host default (it is what the demos, the doc example and
        // `aimdb-codegen` all pass), and the socket then reports `0.0.0.0:port`
        // — a real port paired with an IP that routes nowhere. Advertising that
        // verbatim is worse than either honest option: a gateway that honours
        // the HPAI sends its tunnel data into the void, while `0.0.0.0:0` is the
        // form KNXnet/IP 5.2.3 defines for exactly this case and makes the
        // gateway reply to the datagram's source address instead.
        match socket.local_addr() {
            Some(SocketAddr::V4(addr)) if !addr.ip().is_unspecified() => {
                engine.set_local_endpoint(LocalEndpoint::Explicit {
                    ip: addr.ip().octets(),
                    port: addr.port(),
                });
            }
            _ => engine.set_local_endpoint(LocalEndpoint::Nat),
        }

        drive_connection(
            &mut engine,
            &mut socket,
            gateway,
            &runtime,
            &delay,
            &sink,
            &mut outbound,
        )
        .await;

        // Dropping the socket releases it to the binder, so the next iteration
        // rebinds — `Action::ResetSocket`, honoured neutrally.
        drop(socket);

        let wait_ms = engine.next_deadline().saturating_sub(now_ms());
        delay.sleep(Duration::from_millis(wait_ms)).await;
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use aimdb_core::buffer::BufferCfg;
    use aimdb_core::session::TransportError;
    use aimdb_core::AimDbBuilder;
    use aimdb_tokio_adapter::net::{TokioDelay, TokioNet};
    use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
    use core::future::Future;
    use core::pin::Pin;
    use std::net::Ipv4Addr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// Collects forwarded telegrams; `Sync`, as [`TelegramSink`] requires.
    #[derive(Default)]
    struct VecSink(Mutex<Vec<(String, Vec<u8>)>>);

    impl TelegramSink for VecSink {
        fn deliver(&self, topic: &str, payload: &[u8]) {
            self.0
                .lock()
                .expect("sink mutex")
                .push((topic.into(), payload.into()));
        }
    }

    /// No outbound routes: the command arm never fires.
    async fn no_outbound() -> OutboundRoutes {
        let (db, _runner) = AimDbBuilder::new()
            .runtime(Arc::new(TokioAdapter))
            .build()
            .await
            .expect("build db");
        OutboundRoutes::new(&db, "knx").expect("outbound routes")
    }

    fn runtime() -> Arc<dyn RuntimeOps> {
        Arc::new(TokioAdapter::new().expect("tokio adapter"))
    }

    const RECV_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

    /// `ConnectorBuilder::build` hands the runner a
    /// `Pin<Box<dyn Future<Output = ()> + Send + 'static>>`. Everything that
    /// declares `+ Send` on a return type does so to make this line compile for
    /// a *generic* task.
    #[tokio::test]
    async fn unified_task_is_boxable_as_the_runners_send_future() {
        let task = connection_task(
            TokioNet::udp(Ipv4Addr::LOCALHOST),
            "127.0.0.1:3671".parse().expect("gateway addr"),
            runtime(),
            TokioDelay,
            VecSink::default(),
            no_outbound().await,
        );
        let _boxed: Pin<Box<dyn Future<Output = ()> + Send + 'static>> = Box::pin(task);
    }

    /// The handshake must advertise the socket's real bound address, not the
    /// NAT-style `0.0.0.0:0` some gateways reject. Control HPAI is
    /// `[len, proto, ip(4), port(2)]` at offset 6, so the address is
    /// `buf[8..12]` and the port `buf[12..14]`.
    #[tokio::test]
    async fn unified_task_advertises_the_real_local_endpoint() {
        let gateway = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("bind fake gateway");
        let gateway_addr = gateway.local_addr().expect("gateway addr");

        let task = tokio::spawn(connection_task(
            TokioNet::udp(Ipv4Addr::LOCALHOST),
            gateway_addr,
            runtime(),
            TokioDelay,
            VecSink::default(),
            no_outbound().await,
        ));

        let mut buf = [0u8; 128];
        let (len, from) = tokio::time::timeout(RECV_TIMEOUT, gateway.recv_from(&mut buf))
            .await
            .expect("gateway received no CONNECT_REQUEST")
            .expect("recv_from");

        assert!(len >= 14, "CONNECT_REQUEST should carry both HPAIs");
        assert_ne!(
            &buf[8..12],
            &[0, 0, 0, 0],
            "advertised the NAT-style HPAI: local_addr did not reach the wire"
        );
        assert_eq!(&buf[8..12], &[127, 0, 0, 1], "advertised IP");
        assert_eq!(
            u16::from_be_bytes([buf[12], buf[13]]),
            from.port(),
            "advertised port must be the socket's real bound port"
        );

        task.abort();
    }

    /// The counterpart of the test above, for the bind address everything
    /// actually ships with.
    ///
    /// `Ipv4Addr::UNSPECIFIED` is what the demos, the crate doc example and
    /// `aimdb-codegen` all pass, so `local_addr()` reports `0.0.0.0:<port>`.
    /// That must go out as the NAT HPAI (`0.0.0.0:0`), not as the port paired
    /// with an IP that routes nowhere — a gateway honouring the latter would
    /// send its tunnel data into the void.
    #[tokio::test]
    async fn an_unspecified_bind_address_advertises_the_nat_hpai() {
        let gateway = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("bind fake gateway");
        let gateway_addr = gateway.local_addr().expect("gateway addr");

        let task = tokio::spawn(connection_task(
            TokioNet::udp(Ipv4Addr::UNSPECIFIED),
            gateway_addr,
            runtime(),
            TokioDelay,
            VecSink::default(),
            no_outbound().await,
        ));

        let mut buf = [0u8; 128];
        let (len, _from) = tokio::time::timeout(RECV_TIMEOUT, gateway.recv_from(&mut buf))
            .await
            .expect("gateway received no CONNECT_REQUEST")
            .expect("recv_from");

        assert!(len >= 14, "CONNECT_REQUEST should carry both HPAIs");
        assert_eq!(&buf[8..12], &[0, 0, 0, 0], "NAT HPAI address");
        assert_eq!(
            u16::from_be_bytes([buf[12], buf[13]]),
            0,
            "NAT HPAI must zero the port too: a real port beside 0.0.0.0 is \
             neither an endpoint nor the spec's NAT form"
        );

        task.abort();
    }

    /// Answer the client's CONNECT_REQUEST with channel 7; returns the
    /// client's address.
    async fn accept_connect(gateway: &tokio::net::UdpSocket) -> SocketAddr {
        let mut buf = [0u8; 128];
        let (_, client_addr) = tokio::time::timeout(RECV_TIMEOUT, gateway.recv_from(&mut buf))
            .await
            .expect("no CONNECT_REQUEST")
            .expect("recv_from");
        assert_eq!(u16::from_be_bytes([buf[2], buf[3]]), 0x0205);

        let mut connect_response = vec![0x06, 0x10, 0x02, 0x06, 0x00, 0x14];
        connect_response.extend_from_slice(&[7, 0]);
        connect_response.extend_from_slice(&[0x08, 0x01, 0, 0, 0, 0, 0, 0]);
        connect_response.extend_from_slice(&[0x04, 0x04, 0x02, 0x00]);
        gateway
            .send_to(&connect_response, client_addr)
            .await
            .expect("send CONNECT_RESPONSE");
        client_addr
    }

    /// Wait for a TUNNELING_REQUEST and check it writes 1 to group 1/0/8.
    async fn expect_write_to_1_0_8(gateway: &tokio::net::UdpSocket) {
        let mut buf = [0u8; 128];
        let (len, _) = tokio::time::timeout(RECV_TIMEOUT, gateway.recv_from(&mut buf))
            .await
            .expect("no TUNNELING_REQUEST")
            .expect("recv_from");
        assert_eq!(u16::from_be_bytes([buf[2], buf[3]]), 0x0420);
        assert_eq!(&buf[16..18], &[0x08, 0x08], "cEMI destination = 1/0/8");
        assert_eq!(buf[len - 1], 0x81, "APCI GroupValueWrite | value 1");
    }

    /// A db whose `knx` connector tunnels to `gateway`: record `in` reads group
    /// 1/0/7 and record `out` writes group 1/0/8.
    async fn knx_db(gateway: SocketAddr) -> (aimdb_core::AimDb, aimdb_core::builder::AimDbRunner) {
        let mut builder = AimDbBuilder::new()
            .runtime(Arc::new(TokioAdapter))
            .with_connector(crate::KnxConnector::new(
                TokioNet::udp(Ipv4Addr::LOCALHOST),
                TokioDelay,
                format!("knx://{gateway}"),
            ));
        builder.configure::<u8>("in", |reg| {
            reg.buffer(BufferCfg::SingleLatest)
                .link_from("knx://1/0/7")
                .with_deserializer(|_ctx, data: &[u8]| {
                    data.first().copied().ok_or_else(|| String::from("empty"))
                })
                .finish();
        });
        builder.configure::<u8>("out", |reg| {
            reg.buffer(BufferCfg::SingleLatest)
                .link_to("knx://1/0/8")
                .with_serializer(|_ctx, v: &u8| Ok(vec![*v]))
                .finish();
        });
        builder.build().await.expect("build db")
    }

    /// The connector end to end: a full handshake, an inbound telegram
    /// dispatched into its record with its ACK, and a produced value on the
    /// wire as a group write.
    #[tokio::test]
    async fn telegrams_round_trip_through_records() {
        let gateway = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("bind gateway");
        let (db, runner) = knx_db(gateway.local_addr().expect("gateway addr")).await;
        let mut inbound = db.subscribe::<u8>("in").expect("subscribe");
        let task = tokio::spawn(runner.run());

        let client_addr = accept_connect(&gateway).await;

        // Inbound telegram -> ACK on the wire, value in the record.
        let cemi = [
            0x29, 0x00, 0xBC, 0xE0, 0x00, 0x00, 0x08, 0x07, 0x01, 0x00, 0x81,
        ];
        let total = 6 + 4 + cemi.len() as u16;
        let mut telegram = vec![0x06, 0x10, 0x04, 0x20];
        telegram.extend_from_slice(&total.to_be_bytes());
        telegram.extend_from_slice(&[0x04, 7, 42, 0x00]);
        telegram.extend_from_slice(&cemi);
        gateway
            .send_to(&telegram, client_addr)
            .await
            .expect("send telegram");

        let mut buf = [0u8; 128];
        tokio::time::timeout(RECV_TIMEOUT, gateway.recv_from(&mut buf))
            .await
            .expect("no TUNNELING_ACK")
            .expect("recv_from");
        assert_eq!(u16::from_be_bytes([buf[2], buf[3]]), 0x0421);
        assert_eq!(buf[8], 42, "sequence echoed");

        let value = tokio::time::timeout(RECV_TIMEOUT, inbound.recv())
            .await
            .expect("no telegram reached the record")
            .expect("recv");
        assert_eq!(value, 0x01);

        // Outbound: a produced value reaches the wire.
        db.producer::<u8>("out").expect("producer").produce(1);
        expect_write_to_1_0_8(&gateway).await;

        task.abort();
    }

    /// A value produced while the tunnel is still connecting waits in its
    /// record buffer and goes out once the handshake completes.
    #[tokio::test]
    async fn a_value_produced_during_connect_is_sent_after_the_handshake() {
        let gateway = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("bind gateway");
        let (db, runner) = knx_db(gateway.local_addr().expect("gateway addr")).await;
        db.producer::<u8>("out").expect("producer").produce(1);
        let task = tokio::spawn(runner.run());

        accept_connect(&gateway).await;
        expect_write_to_1_0_8(&gateway).await;

        task.abort();
    }

    /// A real socket that can be told to report no bound address, as an Embassy
    /// stack does whenever `config_v4()` is `None` — DHCP renewal, link flap.
    struct FlappingSocket {
        inner: tokio::net::UdpSocket,
        report_addr: bool,
        fail_recv: bool,
    }

    // Plain `async fn`s: on std the compiler discharges the traits' `+ Send`
    // return bounds, exactly as the real `TokioNet` sockets do.
    impl Datagram for FlappingSocket {
        async fn send_to(&mut self, buf: &[u8], to: SocketAddr) -> TransportResult<()> {
            self.inner
                .send_to(buf, to)
                .await
                .map(|_| ())
                .map_err(|_| TransportError::Io)
        }

        async fn recv_from(&mut self, buf: &mut [u8]) -> TransportResult<(usize, SocketAddr)> {
            if self.fail_recv {
                // Drives the engine to `ResetSocket`, so the task rebinds.
                return Err(TransportError::Io);
            }
            self.inner
                .recv_from(buf)
                .await
                .map_err(|_| TransportError::Io)
        }

        fn local_addr(&self) -> Option<SocketAddr> {
            self.report_addr
                .then(|| self.inner.local_addr().ok())
                .flatten()
        }
    }

    /// Binds a socket that knows its address on the first cycle and, like a
    /// stack mid-DHCP-renewal, does not on the cycles after it.
    #[derive(Default)]
    struct FlappingBinder(AtomicUsize);

    impl DatagramBinder for FlappingBinder {
        type Socket = FlappingSocket;

        async fn bind(&self, port: u16) -> TransportResult<Self::Socket> {
            let cycle = self.0.fetch_add(1, Ordering::SeqCst);
            let inner = tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, port))
                .await
                .map_err(|_| TransportError::Io)?;
            Ok(FlappingSocket {
                inner,
                // Only the first cycle can answer `local_addr`.
                report_addr: cycle == 0,
                // ...and only the first cycle errors, to force the rebind.
                fail_recv: cycle == 0,
            })
        }
    }

    /// Reports `Busy` for the first two binds, then hands over a real socket —
    /// a single-socket binder whose socket another handle is holding.
    struct BusyThenFreeBinder {
        attempts: Arc<AtomicUsize>,
    }

    impl DatagramBinder for BusyThenFreeBinder {
        type Socket = FlappingSocket;

        async fn bind(&self, port: u16) -> TransportResult<Self::Socket> {
            if self.attempts.fetch_add(1, Ordering::SeqCst) < 2 {
                return Err(TransportError::Busy);
            }
            let inner = tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, port))
                .await
                .map_err(|_| TransportError::Io)?;
            Ok(FlappingSocket {
                inner,
                report_addr: true,
                fail_recv: false,
            })
        }
    }

    /// A `Busy` bind must not end the task: it retries, and connects once the
    /// other handle releases the socket.
    ///
    /// `start_paused` so the `BIND_RETRY` sleeps are virtual — the two retries
    /// cost 10 s of tokio's clock and no wall time.
    #[tokio::test(start_paused = true)]
    async fn a_busy_binder_retries_and_recovers() {
        let gateway = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("bind fake gateway");
        let gateway_addr = gateway.local_addr().expect("gateway addr");
        let attempts = Arc::new(AtomicUsize::new(0));

        let task = tokio::spawn(connection_task(
            BusyThenFreeBinder {
                attempts: attempts.clone(),
            },
            gateway_addr,
            runtime(),
            TokioDelay,
            VecSink::default(),
            no_outbound().await,
        ));

        // Must exceed the two `BIND_RETRY` sleeps the task waits out. `RECV_TIMEOUT`
        // would not: it and the first retry share the t=5s deadline, and under a
        // paused clock the timeout wins before the second retry is ever reached.
        const PAST_TWO_RETRIES: std::time::Duration = std::time::Duration::from_secs(60);

        let mut buf = [0u8; 128];
        let (len, _) = tokio::time::timeout(PAST_TWO_RETRIES, gateway.recv_from(&mut buf))
            .await
            .expect("no CONNECT_REQUEST: the task did not retry past Busy")
            .expect("recv_from");

        assert!(
            len >= 14,
            "CONNECT_REQUEST reached the wire after the retries"
        );
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            3,
            "two Busy binds, then the one that succeeded"
        );

        task.abort();
    }

    /// A rebind that cannot learn its address must advertise the NAT-style
    /// HPAI, never the previous cycle's port.
    ///
    /// `engine` outlives the bind loop, so an endpoint set on one cycle would
    /// otherwise persist into the next. The gateway would then reply to a port
    /// nothing is bound to any more and the handshake could never complete —
    /// strictly worse than the `0.0.0.0:0` the explicit HPAI exists to avoid.
    ///
    /// The second request arrives after the engine's reconnect backoff, hence
    /// the wider timeout.
    #[tokio::test]
    async fn a_rebind_that_cannot_learn_its_address_falls_back_to_nat() {
        const BACKOFF_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

        let gateway = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("bind fake gateway");
        let gateway_addr = gateway.local_addr().expect("gateway addr");

        let task = tokio::spawn(connection_task(
            FlappingBinder::default(),
            gateway_addr,
            runtime(),
            TokioDelay,
            VecSink::default(),
            no_outbound().await,
        ));

        // Cycle 1: the socket knows its address, so the HPAI is explicit.
        let mut buf = [0u8; 128];
        let (len, _) = tokio::time::timeout(RECV_TIMEOUT, gateway.recv_from(&mut buf))
            .await
            .expect("gateway received no first CONNECT_REQUEST")
            .expect("recv_from");
        assert!(len >= 14, "CONNECT_REQUEST should carry both HPAIs");
        let first_port = u16::from_be_bytes([buf[12], buf[13]]);
        assert_ne!(first_port, 0, "first cycle should advertise a real port");

        // Cycle 2: the recv error reset the socket and the rebind cannot answer
        // `local_addr`, so the endpoint must fall back rather than persist.
        let mut buf = [0u8; 128];
        let (len, _) = tokio::time::timeout(BACKOFF_TIMEOUT, gateway.recv_from(&mut buf))
            .await
            .expect("gateway received no CONNECT_REQUEST after the rebind")
            .expect("recv_from");
        assert!(len >= 14, "CONNECT_REQUEST should carry both HPAIs");

        let second_port = u16::from_be_bytes([buf[12], buf[13]]);
        assert_ne!(
            second_port, first_port,
            "rebind re-advertised the previous cycle's port: the endpoint went stale"
        );
        assert_eq!(
            &buf[8..12],
            &[0, 0, 0, 0],
            "a rebind with no known address must advertise the NAT-style HPAI"
        );
        assert_eq!(second_port, 0, "NAT-style HPAI carries port 0");

        task.abort();
    }
}
