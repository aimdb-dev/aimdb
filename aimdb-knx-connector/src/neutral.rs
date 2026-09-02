//! Design 052 §3.3 / §4, on real code: **one** KNX connection task, generic
//! over core's runtime-neutral [`DatagramBinder`] and [`Delay`], replacing the
//! two hand-written socket loops in `tokio_client.rs` and `embassy_client.rs`.
//!
//! It exists to settle three questions the design left open:
//!
//! 1. **Is `TunnelIo` usable from generic code?** Only once its `send`
//!    declares `+ Send` on the return type — otherwise the task's future
//!    cannot be boxed as the runner's `Send + 'static`. See
//!    `tunnel::tests::drain_actions_future_is_send_in_generic_code`.
//! 2. **Can a neutral `Datagram` carry the local endpoint?** The KNX handshake
//!    advertises the client's own address (HPAI), and gateways that reject the
//!    NAT-style `0.0.0.0:0` form need the real one. [`Datagram::local_addr`]
//!    supplies it on both runtimes — including Embassy, where the hand-written
//!    client never set it at all.
//! 3. **Can the socket be rebound across reconnects?** The engine's
//!    `Action::ResetSocket` drops the socket and rebinds, so the task takes a
//!    [`DatagramBinder`], not a socket.
//!
//! The clock is `RuntimeOps::now_nanos` (a plain call, per §5.3); only
//! *sleeping* goes through the generic [`Delay`], so nothing is boxed per
//! iteration the way `RuntimeOps::sleep` would be.

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::Future;
use core::net::{IpAddr, SocketAddr};
use core::time::Duration;

use aimdb_core::session::{Datagram, DatagramBinder, Delay, Payload};
use aimdb_core::RuntimeOps;

use crate::tunnel::{
    drain_actions, GroupWrite, LocalEndpoint, TunnelConfig, TunnelEngine, TunnelIo,
};
use crate::GroupAddress;

/// Where parsed telegrams go: an `embassy_sync` channel on the MCU, a
/// `tokio::sync::mpsc` on the host. Non-blocking by contract — a full channel
/// drops rather than stalling the protocol loop.
pub trait TelegramSink {
    /// Enqueue one `(group-address, payload)`. `false` if it was dropped.
    fn try_send(&self, topic: String, payload: Payload) -> bool;
}

/// Where outbound commands come from, as the dual of [`TelegramSink`].
pub trait CommandSource {
    /// Yield the next command. Parks forever once no producer remains, so the
    /// select arm simply goes quiet instead of ending the task.
    fn recv(&mut self) -> impl Future<Output = GroupWrite> + Send + '_;
}

/// The socket-side glue for [`drain_actions`], now written once against
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
    // A plain `async fn` satisfies the trait's `+ Send` return bound: the
    // compiler proves it, exactly as on the Tokio adapter's `ByteStream`.
    // Only the Embassy side needs the force-`Send` newtype.
    async fn send(&mut self, frame: &[u8]) -> bool {
        // Log-and-continue: a transient send error must not tear down the
        // tunnel; a persistently dead send path surfaces through the engine's
        // heartbeat-response timeout.
        self.socket.send_to(frame, self.gateway).await.is_ok()
    }

    fn forward(&mut self, addr: GroupAddress, payload: Vec<u8>) {
        let _ = self.sink.try_send(addr.to_string(), Payload::from(payload));
    }

    fn warn_ack_timeout(&mut self, _seq: u8) {}
}

/// Drive the engine over one socket lifetime; returns when it asks for a reset.
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
        let mut recv_buf = [0u8; 512];

        // Only drain commands while connected: during connect / backoff the
        // arm stays pending, so commands keep queueing and flush once the
        // handshake completes — matching both previous implementations.
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
            // Wake for the engine deadline; `poll` at the loop top fires it.
            Either3::Third(()) => {}
        }
    }
}

/// The unified connection task. One body, both runtimes.
///
/// Binds a socket, advertises its real local endpoint when the stack exposes
/// one, drives the shared [`TunnelEngine`] over that socket's lifetime, then
/// rebinds after the engine's backoff — the same lifecycle both hand-written
/// clients implement today, written once.
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
                delay.sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        // The neutral answer to "the Tokio half reads `local_addr()`, the
        // trait has none". It has one, and Embassy can answer it too.
        if let Some(IpAddr::V4(ip)) = socket.local_addr().map(|a| a.ip()) {
            engine.set_local_endpoint(LocalEndpoint::Explicit {
                ip: ip.octets(),
                port: socket.local_addr().map(|a| a.port()).unwrap_or(0),
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

        // Dropping the socket releases it back to the binder, so the next
        // iteration rebinds — `Action::ResetSocket`, honoured neutrally.
        drop(socket);

        let wait_ms = engine.next_deadline().saturating_sub(now_ms());
        delay.sleep(Duration::from_millis(wait_ms)).await;
    }
}

#[cfg(all(test, feature = "tokio-runtime"))]
mod tests {
    use super::*;
    use aimdb_tokio_adapter::net::{TokioDelay, TokioNet};
    use aimdb_tokio_adapter::TokioAdapter;
    use core::pin::Pin;
    use std::sync::Mutex;

    /// Collects forwarded telegrams; `Sync`, as [`TelegramSink`] requires.
    #[derive(Default)]
    struct VecSink(Mutex<Vec<(String, Payload)>>);

    impl TelegramSink for VecSink {
        fn try_send(&self, topic: String, payload: Payload) -> bool {
            self.0.lock().unwrap().push((topic, payload));
            true
        }
    }

    /// No outbound producer: the command arm simply never fires.
    struct NoCommands;

    impl CommandSource for NoCommands {
        fn recv(&mut self) -> impl Future<Output = GroupWrite> + Send + '_ {
            core::future::pending()
        }
    }

    fn runtime() -> Arc<dyn RuntimeOps> {
        Arc::new(TokioAdapter::new().expect("tokio adapter"))
    }

    /// **The boxing contract.** `ConnectorBuilder::build` hands the runner
    /// `Pin<Box<dyn Future<Output = ()> + Send + 'static>>`. The whole reason
    /// `TunnelIo::send` and the `Datagram`/`Delay` methods declare `+ Send` on
    /// their return types is so this line compiles for a *generic* task.
    #[test]
    fn unified_task_is_boxable_as_the_runners_send_future() {
        let task = connection_task(
            TokioNet::udp("127.0.0.1:0"),
            "127.0.0.1:3671".parse().unwrap(),
            runtime(),
            TokioDelay,
            VecSink::default(),
            NoCommands,
        );
        let _boxed: Pin<Box<dyn Future<Output = ()> + Send + 'static>> = Box::pin(task);
    }

    /// **Q2, answered on the wire.** The KNX handshake advertises the client's
    /// own endpoint (HPAI). The Tokio half reads it from `local_addr()` after
    /// binding; the verification flagged that core's `Datagram` sketch had no
    /// such method, so unifying the task would silently downgrade every Tokio
    /// deployment to the NAT-style `0.0.0.0:0` form that some gateways reject.
    ///
    /// With `Datagram::local_addr` in the trait, the unified task advertises
    /// the real address. This drives it against a real UDP "gateway" socket
    /// and inspects the bytes: control HPAI is `[len, proto, ip(4), port(2)]`
    /// at offset 6, so the address is `req[8..12]` and the port `req[12..14]`.
    #[tokio::test]
    async fn unified_task_advertises_the_real_local_endpoint() {
        let gateway = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("bind fake gateway");
        let gateway_addr = gateway.local_addr().expect("gateway addr");

        let task = tokio::spawn(connection_task(
            TokioNet::udp("127.0.0.1:0"),
            gateway_addr,
            runtime(),
            TokioDelay,
            VecSink::default(),
            NoCommands,
        ));

        let mut buf = [0u8; 128];
        let (len, from) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            gateway.recv_from(&mut buf),
        )
        .await
        .expect("gateway received no CONNECT_REQUEST")
        .expect("recv_from");

        assert!(len >= 14, "CONNECT_REQUEST should carry both HPAIs");
        let advertised_ip = &buf[8..12];
        let advertised_port = u16::from_be_bytes([buf[12], buf[13]]);

        assert_ne!(
            advertised_ip,
            &[0, 0, 0, 0],
            "the task advertised the NAT-style HPAI: `Datagram::local_addr` \
             did not reach the wire"
        );
        assert_eq!(advertised_ip, &[127, 0, 0, 1], "advertised IP");
        assert_eq!(
            advertised_port,
            from.port(),
            "advertised port must be the socket's real bound port"
        );

        task.abort();
    }

    /// **§5.2, exercised end to end.** The unified task, on Tokio, moving real
    /// telegrams through the *same* `embassy_sync` channel type the Embassy
    /// half uses — against a real UDP gateway, through a full handshake.
    ///
    /// This is the claim "the Tokio half simply adopts it", executed rather
    /// than asserted. It also links `critical-section/std`: without the
    /// `tokio-runtime` feature pulling that in, this binary would fail with
    /// `undefined symbol: _critical_section_1_0_acquire`.
    #[tokio::test]
    async fn shared_embassy_channels_carry_telegrams_on_tokio() {
        use super::shared_channel::{ChannelCommands, ChannelSink};
        use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
        use embassy_sync::channel::Channel;

        const N: usize = 8;
        // Leaked so the borrows are `'static`, as a `StaticCell` would give on
        // the MCU (design 052 §5.2's "one-line allocation choice").
        let inbound: &'static Channel<CriticalSectionRawMutex, (String, Payload), N> =
            Box::leak(Box::new(Channel::new()));
        let commands: &'static Channel<CriticalSectionRawMutex, GroupWrite, N> =
            Box::leak(Box::new(Channel::new()));

        let gateway = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let gateway_addr = gateway.local_addr().unwrap();

        let task = tokio::spawn(connection_task(
            TokioNet::udp("127.0.0.1:0"),
            gateway_addr,
            runtime(),
            TokioDelay,
            ChannelSink::<N>(inbound.sender()),
            ChannelCommands::<N>(commands.receiver()),
        ));

        let recv_timeout = std::time::Duration::from_secs(5);
        let mut buf = [0u8; 1024];

        // Handshake.
        let (len, client_addr) = tokio::time::timeout(recv_timeout, gateway.recv_from(&mut buf))
            .await
            .expect("no CONNECT_REQUEST")
            .unwrap();
        assert_eq!(u16::from_be_bytes([buf[2], buf[3]]), 0x0205);
        let _ = len;
        let mut connect_response = vec![0x06, 0x10, 0x02, 0x06, 0x00, 0x14];
        connect_response.extend_from_slice(&[7, 0]);
        connect_response.extend_from_slice(&[0x08, 0x01, 0, 0, 0, 0, 0, 0]);
        connect_response.extend_from_slice(&[0x04, 0x04, 0x02, 0x00]);
        gateway
            .send_to(&connect_response, client_addr)
            .await
            .unwrap();

        // Inbound telegram -> ACK on the wire, payload on the shared channel.
        let cemi = [
            0x29, 0x00, 0xBC, 0xE0, 0x00, 0x00, 0x08, 0x07, 0x01, 0x00, 0x81,
        ];
        let total = 6 + 4 + cemi.len() as u16;
        let mut telegram = vec![0x06, 0x10, 0x04, 0x20];
        telegram.extend_from_slice(&total.to_be_bytes());
        telegram.extend_from_slice(&[0x04, 7, 42, 0x00]);
        telegram.extend_from_slice(&cemi);
        gateway.send_to(&telegram, client_addr).await.unwrap();

        let (len, _) = tokio::time::timeout(recv_timeout, gateway.recv_from(&mut buf))
            .await
            .expect("no TUNNELING_ACK")
            .unwrap();
        assert_eq!(u16::from_be_bytes([buf[2], buf[3]]), 0x0421);
        assert_eq!(buf[8], 42, "sequence echoed");
        let _ = len;

        let (topic, payload) = tokio::time::timeout(recv_timeout, inbound.receive())
            .await
            .expect("no telegram reached the embassy-sync channel");
        assert_eq!(topic, "1/0/7");
        assert_eq!(&payload[..], &[0x01]);

        // Outbound: a command through the shared channel reaches the wire.
        let mut data = heapless::Vec::new();
        data.push(0x01).unwrap();
        commands
            .send(GroupWrite {
                group_addr: "1/0/8".parse().unwrap(),
                data,
            })
            .await;
        let (len, _) = tokio::time::timeout(recv_timeout, gateway.recv_from(&mut buf))
            .await
            .expect("no TUNNELING_REQUEST")
            .unwrap();
        assert_eq!(u16::from_be_bytes([buf[2], buf[3]]), 0x0420);
        assert_eq!(&buf[16..18], &[0x08, 0x08], "cEMI destination = 1/0/8");
        assert_eq!(buf[len - 1], 0x81, "APCI GroupValueWrite | value 1");

        task.abort();
    }
}

/// Design 052 §5.2, exercised: the same `embassy_sync` channel type the
/// Embassy half uses today, wired into the neutral task on **Tokio**.
///
/// `CriticalSectionRawMutex` is the only `Sync` raw mutex `embassy-sync`
/// offers (`NoopRawMutex` is `!Sync`, so it cannot back a shared channel), and
/// it resolves to `_critical_section_1_0_acquire`/`_release` — extern symbols
/// the final binary must supply. The `tokio-runtime` feature enables
/// `critical-section/std` so downstream std users never see that link error.
#[cfg(feature = "tokio-runtime")]
pub mod shared_channel {
    use super::{CommandSource, GroupWrite, Payload, TelegramSink};
    use alloc::string::String;
    use core::future::Future;
    use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
    use embassy_sync::channel::{Receiver, Sender};

    /// Outbound half of the inbound-telegram channel.
    pub struct ChannelSink<'a, const N: usize>(
        pub Sender<'a, CriticalSectionRawMutex, (String, Payload), N>,
    );

    impl<const N: usize> TelegramSink for ChannelSink<'_, N> {
        fn try_send(&self, topic: String, payload: Payload) -> bool {
            self.0.try_send((topic, payload)).is_ok()
        }
    }

    /// Inbound half of the outbound-command channel.
    pub struct ChannelCommands<'a, const N: usize>(
        pub Receiver<'a, CriticalSectionRawMutex, GroupWrite, N>,
    );

    impl<const N: usize> CommandSource for ChannelCommands<'_, N> {
        fn recv(&mut self) -> impl Future<Output = GroupWrite> + Send + '_ {
            self.0.receive()
        }
    }
}
