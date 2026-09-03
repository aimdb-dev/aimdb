//! One KNX connection task, generic over core's [`DatagramBinder`] and
//! [`Delay`], replacing the two hand-written socket loops.
//!
//! The clock stays `RuntimeOps::now_nanos`, a plain call; only *sleeping* goes
//! through [`Delay`], so nothing is boxed per loop iteration.
//!
//! The `embassy-sync` and `embassy-futures` types below are executor-independent
//! — neither pulls an executor, and both build on std — so they back this task
//! on either runtime.

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::Future;
use core::net::SocketAddr;
use core::time::Duration;

use aimdb_core::session::{Datagram, DatagramBinder, Delay, Payload};
use aimdb_core::{log_debug, log_error, log_warn, RuntimeOps};

use crate::tunnel::{
    drain_actions, GroupWrite, LocalEndpoint, TunnelConfig, TunnelEngine, TunnelIo,
};
use crate::GroupAddress;

/// Backoff before retrying a bind that failed.
const BIND_RETRY: Duration = Duration::from_secs(5);
/// Per-datagram receive buffer; a KNXnet/IP frame fits comfortably.
const RECV_BUF: usize = 512;

/// Where parsed telegrams go — an `embassy_sync` channel on either runtime.
///
/// Non-blocking by contract: a full sink drops rather than stalling the
/// protocol loop.
pub trait TelegramSink {
    /// Enqueue one `(group-address, payload)`. `false` if it was dropped.
    fn try_send(&self, topic: String, payload: Payload) -> bool;
}

/// Where outbound commands come from, the dual of [`TelegramSink`].
pub trait CommandSource {
    /// Yield the next command. Parks forever once no producer remains, so the
    /// select arm goes quiet instead of ending the task.
    fn recv(&mut self) -> impl Future<Output = GroupWrite> + Send + '_;
}

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
        if !self.sink.try_send(addr.to_string(), Payload::from(payload)) {
            log_warn!("KNX inbound: dropping telegram for {} (sink full)", addr);
        }
    }

    fn warn_ack_timeout(&mut self, _seq: u8) {
        log_warn!("KNX outbound: no ACK for sequence {}", _seq);
    }
}

/// Drive the engine over one socket's lifetime; returns when it asks for a reset.
async fn drive_connection<U, D, S, C>(
    engine: &mut TunnelEngine,
    socket: &mut U,
    gateway: SocketAddr,
    runtime: &Arc<dyn RuntimeOps>,
    delay: &D,
    sink: &S,
    commands: &mut C,
) where
    U: Datagram + Send,
    D: Delay,
    S: TelegramSink + Sync,
    C: CommandSource,
{
    // Executor-independent despite the name: `embassy-futures` has no
    // dependencies and its select is pure `core::task`.
    use embassy_futures::select::{select3, Either3};

    let now_ms = || runtime.now_nanos() / 1_000_000;

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

        // Only drain commands while connected: during connect and backoff the
        // arm stays pending, so commands queue and flush once the handshake
        // completes — as both hand-written clients do.
        let connected = engine.is_connected();
        let cmd_arm = async {
            if connected {
                commands.recv().await
            } else {
                core::future::pending().await
            }
        };

        match select3(socket.recv_from(&mut recv_buf), cmd_arm, deadline).await {
            Either3::First(Ok((len, _peer))) => {
                engine.handle_datagram(&recv_buf[..len], now_ms());
            }
            Either3::First(Err(_)) => engine.handle_socket_error(now_ms()),
            Either3::Second(cmd) => {
                let _ = engine.handle_command(cmd, now_ms());
            }
            // Woken for the engine deadline; `poll` at the loop top fires it.
            Either3::Third(()) => {}
        }
    }
}

/// The unified connection task: one body, both runtimes.
///
/// Binds a socket, advertises its real local endpoint when the stack exposes
/// one, drives the shared [`TunnelEngine`] over that socket's lifetime, then
/// rebinds after the engine's backoff.
pub async fn connection_task<B, D, S, C>(
    binder: B,
    gateway: SocketAddr,
    runtime: Arc<dyn RuntimeOps>,
    delay: D,
    sink: S,
    mut commands: C,
) where
    B: DatagramBinder,
    D: Delay,
    S: TelegramSink + Sync,
    C: CommandSource,
{
    let now_ms = || runtime.now_nanos() / 1_000_000;
    let mut engine = TunnelEngine::new(TunnelConfig::default(), now_ms());

    loop {
        let mut socket = match binder.bind(0).await {
            Ok(socket) => socket,
            Err(_) => {
                log_error!("KNX bind failed; retrying");
                delay.sleep(BIND_RETRY).await;
                continue;
            }
        };

        // The handshake advertises the client's own endpoint (HPAI). Gateways
        // that reject the NAT-style `0.0.0.0:0` form need the real address.
        if let Some(SocketAddr::V4(addr)) = socket.local_addr() {
            engine.set_local_endpoint(LocalEndpoint::Explicit {
                ip: addr.ip().octets(),
                port: addr.port(),
            });
        }

        drive_connection(
            &mut engine,
            &mut socket,
            gateway,
            &runtime,
            &delay,
            &sink,
            &mut commands,
        )
        .await;

        // Dropping the socket releases it to the binder, so the next iteration
        // rebinds — `Action::ResetSocket`, honoured neutrally.
        drop(socket);

        let wait_ms = engine.next_deadline().saturating_sub(now_ms());
        delay.sleep(Duration::from_millis(wait_ms)).await;
    }
}

/// Channel bridges over `embassy_sync`, which is executor-independent, so the
/// same types back the task on both runtimes.
#[cfg(any(feature = "tokio-runtime", feature = "embassy-runtime"))]
pub mod shared_channel {
    use super::{CommandSource, GroupWrite, Payload, TelegramSink};
    use alloc::string::String;
    use core::future::Future;
    use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
    use embassy_sync::channel::{Receiver, Sender};

    /// Sending half of the inbound-telegram channel.
    pub struct ChannelSink<'a, const N: usize>(
        pub Sender<'a, CriticalSectionRawMutex, (String, Payload), N>,
    );

    impl<const N: usize> TelegramSink for ChannelSink<'_, N> {
        fn try_send(&self, topic: String, payload: Payload) -> bool {
            self.0.try_send((topic, payload)).is_ok()
        }
    }

    /// Receiving half of the outbound-command channel.
    pub struct ChannelCommands<'a, const N: usize>(
        pub Receiver<'a, CriticalSectionRawMutex, GroupWrite, N>,
    );

    impl<const N: usize> CommandSource for ChannelCommands<'_, N> {
        fn recv(&mut self) -> impl Future<Output = GroupWrite> + Send + '_ {
            self.0.receive()
        }
    }
}

#[cfg(all(test, feature = "tokio-runtime"))]
mod tests {
    use super::*;
    use aimdb_tokio_adapter::net::{TokioDelay, TokioNet};
    use aimdb_tokio_adapter::TokioAdapter;
    use core::pin::Pin;
    use std::net::Ipv4Addr;
    use std::sync::Mutex;

    /// Collects forwarded telegrams; `Sync`, as [`TelegramSink`] requires.
    #[derive(Default)]
    struct VecSink(Mutex<Vec<(String, Payload)>>);

    impl TelegramSink for VecSink {
        fn try_send(&self, topic: String, payload: Payload) -> bool {
            self.0.lock().expect("sink mutex").push((topic, payload));
            true
        }
    }

    /// No outbound producer: the command arm never fires.
    struct NoCommands;

    impl CommandSource for NoCommands {
        fn recv(&mut self) -> impl Future<Output = GroupWrite> + Send + '_ {
            core::future::pending()
        }
    }

    fn runtime() -> Arc<dyn RuntimeOps> {
        Arc::new(TokioAdapter::new().expect("tokio adapter"))
    }

    const RECV_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

    /// `ConnectorBuilder::build` hands the runner a
    /// `Pin<Box<dyn Future<Output = ()> + Send + 'static>>`. Everything that
    /// declares `+ Send` on a return type does so to make this line compile for
    /// a *generic* task.
    #[test]
    fn unified_task_is_boxable_as_the_runners_send_future() {
        let task = connection_task(
            TokioNet::udp(Ipv4Addr::LOCALHOST),
            "127.0.0.1:3671".parse().expect("gateway addr"),
            runtime(),
            TokioDelay,
            VecSink::default(),
            NoCommands,
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
            NoCommands,
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

    /// The unified task on Tokio, moving real telegrams through the *same*
    /// `embassy_sync` channel types the MCU uses: a full handshake, an inbound
    /// telegram with its ACK, and an outbound command.
    #[tokio::test]
    async fn shared_embassy_channels_carry_telegrams_on_tokio() {
        use super::shared_channel::{ChannelCommands, ChannelSink};
        use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
        use embassy_sync::channel::Channel;

        const N: usize = 8;
        // Leaked for `'static` borrows, as a `StaticCell` gives on the MCU.
        let inbound: &'static Channel<CriticalSectionRawMutex, (String, Payload), N> =
            Box::leak(Box::new(Channel::new()));
        let commands: &'static Channel<CriticalSectionRawMutex, GroupWrite, N> =
            Box::leak(Box::new(Channel::new()));

        let gateway = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("bind gateway");
        let gateway_addr = gateway.local_addr().expect("gateway addr");

        let task = tokio::spawn(connection_task(
            TokioNet::udp(Ipv4Addr::LOCALHOST),
            gateway_addr,
            runtime(),
            TokioDelay,
            ChannelSink::<N>(inbound.sender()),
            ChannelCommands::<N>(commands.receiver()),
        ));

        let mut buf = [0u8; 1024];

        // Handshake.
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

        // Inbound telegram -> ACK on the wire, payload on the shared channel.
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

        tokio::time::timeout(RECV_TIMEOUT, gateway.recv_from(&mut buf))
            .await
            .expect("no TUNNELING_ACK")
            .expect("recv_from");
        assert_eq!(u16::from_be_bytes([buf[2], buf[3]]), 0x0421);
        assert_eq!(buf[8], 42, "sequence echoed");

        let (topic, payload) = tokio::time::timeout(RECV_TIMEOUT, inbound.receive())
            .await
            .expect("no telegram reached the embassy-sync channel");
        assert_eq!(topic, "1/0/7");
        assert_eq!(&payload[..], &[0x01]);

        // Outbound: a command through the shared channel reaches the wire.
        let mut data = heapless::Vec::new();
        data.push(0x01).expect("push");
        commands
            .send(GroupWrite {
                group_addr: "1/0/8".parse().expect("group address"),
                data,
            })
            .await;

        let (len, _) = tokio::time::timeout(RECV_TIMEOUT, gateway.recv_from(&mut buf))
            .await
            .expect("no TUNNELING_REQUEST")
            .expect("recv_from");
        assert_eq!(u16::from_be_bytes([buf[2], buf[3]]), 0x0420);
        assert_eq!(&buf[16..18], &[0x08, 0x08], "cEMI destination = 1/0/8");
        assert_eq!(buf[len - 1], 0x81, "APCI GroupValueWrite | value 1");

        task.abort();
    }
}
