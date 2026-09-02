//! Design 052 §3.1, settled on real sockets: **can the single-accept
//! [`StreamListener`] contract back the Embassy N-socket pool without losing
//! accept concurrency?**
//!
//! The verification of design 052 flagged this as the one place where the
//! proposed trait set looked unable to express existing behaviour. It can, and
//! the reason is mechanical: `embassy_net::tcp::TcpSocket::accept` is a
//! synchronous `listen()` plus a bare `poll_fn`, so **dropping the future does
//! not un-listen the socket** — but *re-entering* `accept()` on a listening
//! socket is an `InvalidState` error, and the `abort()` that makes it
//! re-enterable is what drops the `LISTEN`.
//!
//! So the two implementations differ only in whether the pending accepts are
//! kept:
//!
//! - `EmbassyNet::listen::<N>` stores one accept future per slot and consumes
//!   only the one that completes (`pool_keeps_every_slot_listening`);
//! - `NaivePooledListener` below re-creates all `N` each call, aborting first
//!   (`naive_pool_drops_the_syn_between_accepts`).
//!
//! Both run over the same two crossover-wired `embassy-net` stacks as
//! `embassy_loopback.rs`, and both are driven in the exact shape core's
//! `serve` loop uses: one `accept().await` at a time.
#![cfg(feature = "_test-embassy-loopback")]

extern crate alloc;

use core::future::Future;

use aimdb_core::session::{
    ByteStream, StreamDialer, StreamListener, TransportError, TransportResult,
};
use aimdb_embassy_adapter::net::{EmbassyNet, EmbassyTcpStream, TcpSocketSlot};
use alloc::sync::Arc;
use embassy_net::tcp::TcpSocket;
use embassy_net::{
    Config, IpListenEndpoint, Ipv4Address, Ipv4Cidr, Stack, StaticConfigV4,
};
use embassy_net_driver_channel as ch;
use embassy_net_driver_channel::driver::{HardwareAddress, LinkState};

// ---------------------------------------------------------------------------
// Host stubs + two crossover stacks (same rig as `embassy_loopback.rs`).
// ---------------------------------------------------------------------------

#[defmt::global_logger]
struct HostTestLogger;
unsafe impl defmt::Logger for HostTestLogger {
    fn acquire() {}
    unsafe fn flush() {}
    unsafe fn release() {}
    unsafe fn write(_bytes: &[u8]) {}
}
#[defmt::panic_handler]
fn defmt_panic() -> ! {
    core::panic!("defmt panic in host test")
}
defmt::timestamp!("{=u64}", 0u64);

struct HostClock;
impl embassy_time_driver::Driver for HostClock {
    fn now(&self) -> u64 {
        use std::sync::OnceLock;
        use std::time::Instant;
        static START: OnceLock<Instant> = OnceLock::new();
        let start = START.get_or_init(Instant::now);
        (start.elapsed().as_micros() * u128::from(embassy_time_driver::TICK_HZ) / 1_000_000) as u64
    }
    fn schedule_wake(&self, _at: u64, waker: &core::task::Waker) {
        waker.wake_by_ref();
    }
}
embassy_time_driver::time_driver_impl!(static HOST_CLOCK: HostClock = HostClock);

const MTU: usize = 1514;
const SERVER_IP: Ipv4Address = Ipv4Address::new(192, 168, 0, 1);
const CLIENT_IP: Ipv4Address = Ipv4Address::new(192, 168, 0, 2);

type ChState = ch::State<MTU, 4, 4>;

fn leak<T>(v: T) -> &'static mut T {
    alloc::boxed::Box::leak(alloc::boxed::Box::new(v))
}

fn buf() -> &'static mut [u8] {
    alloc::boxed::Box::leak(alloc::vec![0u8; 1024].into_boxed_slice())
}

fn make_stack(
    ip: Ipv4Address,
    seed: u64,
) -> (
    Stack<'static>,
    embassy_net::Runner<'static, ch::Device<'static, MTU>>,
    ch::Runner<'static, MTU>,
) {
    let state: &'static mut ChState = leak(ch::State::new());
    let (ch_runner, device) = ch::new(state, HardwareAddress::Ip);
    let config = Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(ip, 24),
        gateway: None,
        dns_servers: heapless::Vec::new(),
    });
    let resources = leak(embassy_net::StackResources::<8>::new());
    let (stack, net_runner) = embassy_net::new(device, config, resources, seed);
    (stack, net_runner, ch_runner)
}

async fn cable(mut tx: ch::TxRunner<'static, MTU>, mut rx: ch::RxRunner<'static, MTU>) -> ! {
    loop {
        let tx_slot = tx.tx_buf().await;
        let len = tx_slot.len();
        let mut rx_slot = rx.rx_buf().await;
        rx_slot[..len].copy_from_slice(&tx_slot[..len]);
        tx_slot.tx_done();
        rx_slot.rx_done(len);
    }
}

/// Run `foreground` while both stacks are polled, with a wall-clock watchdog.
/// Returns `Err` on watchdog expiry instead of panicking, so a test can assert
/// *that* a design hangs rather than dying at the outer CI timeout.
fn drive<Fut, F>(foreground: F) -> Result<(), &'static str>
where
    Fut: Future<Output = ()>,
    F: FnOnce(Stack<'static>, Stack<'static>) -> Fut,
{
    use core::future::poll_fn;
    use core::task::Poll;
    use std::time::{Duration, Instant};

    use futures::future::{join4, select, Either};
    use futures::pin_mut;

    const WATCHDOG: Duration = Duration::from_secs(5);

    let (server_stack, mut server_net, server_ch) = make_stack(SERVER_IP, 0x1111_2222);
    let (client_stack, mut client_net, client_ch) = make_stack(CLIENT_IP, 0x3333_4444);

    let (server_state, server_rx, server_tx) = server_ch.split();
    let (client_state, client_rx, client_tx) = client_ch.split();
    server_state.set_link_state(LinkState::Up);
    client_state.set_link_state(LinkState::Up);

    let background = join4(
        server_net.run(),
        client_net.run(),
        cable(server_tx, client_rx),
        cable(client_tx, server_rx),
    );
    let foreground = foreground(server_stack, client_stack);

    futures::executor::block_on(async {
        pin_mut!(foreground);
        pin_mut!(background);
        let session = select(foreground, background);
        pin_mut!(session);

        let deadline = Instant::now() + WATCHDOG;
        let watchdog = poll_fn(move |cx| {
            if Instant::now() >= deadline {
                Poll::Ready(())
            } else {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        });
        pin_mut!(watchdog);

        match select(session, watchdog).await {
            Either::Left((Either::Left(_), _)) => Ok(()),
            Either::Left((Either::Right(_), _)) => Err("background ended before the test"),
            Either::Right(_) => Err("watchdog: foreground stuck"),
        }
    })
}

/// The server's address as `StreamDialer::connect` takes it: a host string the
/// **adapter** resolves, never an `embassy_net` type the connector would name.
const SERVER_HOST: &str = "192.168.0.1";

/// Round-trip one payload over two neutral [`ByteStream`]s, proving the
/// accepted socket is a live full-duplex connection and not just a completed
/// handshake.
async fn roundtrip(server: &mut EmbassyTcpStream, client: &mut EmbassyTcpStream, tag: &[u8]) {
    client.write_all(tag).await.expect("client write");
    client.flush().await.expect("client flush");

    let mut got = [0u8; 16];
    let n = server.read(&mut got).await.expect("server read");
    assert_eq!(&got[..n], tag, "client -> server bytes");

    server.write_all(b"pong").await.expect("server write");
    server.flush().await.expect("server flush");

    let n = client.read(&mut got).await.expect("client read");
    assert_eq!(&got[..n], b"pong", "server -> client bytes");
}

// ---------------------------------------------------------------------------
// The contrast: a pool that re-creates (and therefore cancels) its accepts.
// ---------------------------------------------------------------------------

/// The obvious reading of `StreamListener`: build `N` accept futures per call,
/// race them, drop the losers. `TcpSocket::accept` cannot be re-entered on a
/// listening socket, so each call must `abort()` first — which is exactly what
/// takes the other slots out of `LISTEN` between calls.
struct NaivePooledListener<const N: usize> {
    local_endpoint: IpListenEndpoint,
    slots: [Arc<TcpSocketSlot>; N],
}

impl<const N: usize> NaivePooledListener<N> {
    fn new(
        stack: Stack<'static>,
        local_endpoint: impl Into<IpListenEndpoint>,
        rx: [&'static mut [u8]; N],
        tx: [&'static mut [u8]; N],
    ) -> Self {
        let mut rx = rx.into_iter();
        let mut tx = tx.into_iter();
        let slots = core::array::from_fn(|_| {
            Arc::new(TcpSocketSlot::new(TcpSocket::new(
                stack,
                rx.next().unwrap(),
                tx.next().unwrap(),
            )))
        });
        Self {
            local_endpoint: local_endpoint.into(),
            slots,
        }
    }

    /// One accept, racing `N` freshly-created futures and dropping the losers.
    async fn accept(&mut self) -> TransportResult<TcpSocket<'static>> {
        use futures::future::{select_all, FutureExt};

        let endpoint = self.local_endpoint;
        let futs: alloc::vec::Vec<_> = self
            .slots
            .iter()
            .map(|slot| {
                let slot = slot.clone();
                async move {
                    let mut socket = slot.acquire().await;
                    // Required to make `accept()` re-enterable; also drops any
                    // `LISTEN` this socket was left in by a previous call.
                    socket.abort();
                    match socket.accept(endpoint).await {
                        Ok(()) => Ok(socket),
                        Err(_) => {
                            socket.abort();
                            slot.put(socket);
                            Err(TransportError::Io)
                        }
                    }
                }
                .boxed_local()
            })
            .collect();

        let (result, _idx, _rest) = select_all(futs).await;
        result
    }
}

// ---------------------------------------------------------------------------
// The tests.
// ---------------------------------------------------------------------------

/// **Q1, answered.** Two clients dial the same port. The server accepts them
/// one at a time through `StreamListener::accept(&mut self)` — the exact shape
/// core's `serve` loop uses — and critically, the second client connects
/// *after* the first accept has already returned, while the server is busy
/// doing something else.
///
/// If the pool kept only one socket in `LISTEN`, that second SYN would meet a
/// closed port and be RST'd, so the second `connect()` would fail. It does not:
/// storing the pending accepts keeps every other slot listening.
#[test]
fn pool_keeps_every_slot_listening_between_accepts() {
    let outcome = drive(|server_stack, client_stack| async move {
        let mut listener =
            EmbassyNet::listen::<2>(server_stack, 7101u16, [buf(), buf()], [buf(), buf()]);
        let dialer_a = EmbassyNet::tcp(client_stack, buf(), buf());
        let dialer_b = EmbassyNet::tcp(client_stack, buf(), buf());

        // Accept #1 arms *both* slots and returns as soon as A lands.
        let (accepted_a, client_a) = futures::join!(
            async { listener.accept().await.expect("accept A") },
            async { dialer_a.connect(SERVER_HOST, 7101).await.expect("dial A") },
        );
        let (mut server_a, peer_a) = accepted_a;
        let mut client_a = client_a;
        assert!(
            peer_a.peer_addr.is_some(),
            "StreamListener::accept should carry peer metadata"
        );

        // Prove connection A is live before B is even attempted, so the second
        // dial genuinely happens while the server sits between accepts.
        roundtrip(&mut server_a, &mut client_a, b"aaa").await;

        // B dials into that window. Slot 1 must still be in LISTEN.
        let mut client_b = dialer_b.connect(SERVER_HOST, 7101).await.expect(
            "second SYN was refused: the pool did not keep slot 1 listening between accepts",
        );

        // Only now does the server come back for the second connection.
        let (mut server_b, _) = listener.accept().await.expect("accept B");
        roundtrip(&mut server_b, &mut client_b, b"bbb").await;

        // And both remain independently usable afterwards.
        roundtrip(&mut server_a, &mut client_a, b"a2").await;
    });
    assert_eq!(
        outcome,
        Ok(()),
        "a single-accept StreamListener over a stored-accept pool should serve both clients"
    );
}

/// **The contrast that makes the above a finding rather than a coincidence.**
///
/// Same sockets, same single-accept contract, same scenario — but this pool
/// re-creates its `N` accept futures on every call and drops the losers.
/// `TcpSocket::accept` cannot be re-entered on a socket that is already
/// listening, so each call must `abort()` first, and that abort takes slot 1
/// out of `LISTEN` the moment accept #1 returns. B's SYN then lands on a
/// closed port.
#[test]
fn naive_pool_loses_the_syn_that_arrives_between_accepts() {
    let outcome = drive(|server_stack, client_stack| async move {
        let mut listener =
            NaivePooledListener::<2>::new(server_stack, 7102u16, [buf(), buf()], [buf(), buf()]);
        let dialer_a = EmbassyNet::tcp(client_stack, buf(), buf());
        let dialer_b = EmbassyNet::tcp(client_stack, buf(), buf());

        let (socket_a, _client_a) = futures::join!(
            async { listener.accept().await.expect("accept A") },
            async { dialer_a.connect(SERVER_HOST, 7102).await.expect("dial A") },
        );
        let _keep_a = socket_a;

        // B dials while the server is between accepts. Slot 1 was aborted out
        // of LISTEN when accept #1 raced and dropped it, so this is refused.
        let refused = dialer_b.connect(SERVER_HOST, 7102).await;
        assert_eq!(
            refused.err(),
            Some(TransportError::Io),
            "the naive pool is expected to lose this SYN; if it now succeeds, \
             `TcpSocket::accept` cancellation semantics changed and the stored-\
             accept design in `EmbassyNet::listen` can be simplified"
        );
    });
    assert_eq!(outcome, Ok(()), "the naive-pool scenario should run to completion");
}
