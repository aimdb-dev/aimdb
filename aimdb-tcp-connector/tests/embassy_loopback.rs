//! Runtime smoke for the Embassy TCP half (feature `_test-embassy-loopback`).
//!
//! The transport is welded to a concrete `embassy_net::tcp::TcpSocket` with no
//! seam for a fake, so socket recycling and waker handoff can only be exercised
//! over a real stack. Two `embassy-net` stacks wired by an in-memory
//! `embassy-net-driver-channel` crossover drive the real
//! `TcpListener`/`TcpDialer`/`TcpConnection` triple under `block_on`:
//!
//! - recycle: accept -> exchange -> drop -> re-accept (`recycle_then_reaccept`);
//! - concurrency: two sessions, each with its own accept slot, at once
//!   (`two_concurrent_sessions`);
//! - redial: after a failed connect and after a dropped link
//!   (`dialer_redials_after_failure_and_drop`).
#![cfg(feature = "_test-embassy-loopback")]

extern crate alloc;

use core::future::Future;

use aimdb_core::session::{Connection, Dialer, Listener};
use aimdb_tcp_connector::{TcpDialer, TcpListener};
use embassy_net::{Config, IpAddress, IpEndpoint, Ipv4Address, Ipv4Cidr, Stack, StaticConfigV4};
use embassy_net_driver_channel as ch;
use embassy_net_driver_channel::driver::{HardwareAddress, LinkState};

// No-op defmt logger + panic handler so the binary links (the adapter vtable and
// smoltcp reference the defmt transport). Logger/panic half of `host_test_stubs!`;
// its time driver is replaced below.
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
// smoltcp logs via defmt 0.3 and so references `_defmt_timestamp`; supply one.
defmt::timestamp!("{=u64}", 0u64);

// Real wall-clock host time driver. A frozen `now()` (as in `host_test_stubs!`)
// stalls embassy-net's delayed-ACK timer, so the first `flush()` — wait-for-ACK,
// what `TcpConnection::send` does — never returns. Wakes stay immediate since
// `block_on` busy-polls anyway.
struct HostClock;
impl embassy_time_driver::Driver for HostClock {
    fn now(&self) -> u64 {
        use std::sync::OnceLock;
        use std::time::Instant;
        static START: OnceLock<Instant> = OnceLock::new();
        let start = START.get_or_init(Instant::now);
        // Scale wall-clock micros to the driver's *actual* tick rate
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

/// A leaked `'static` socket buffer, as the connector constructors require.
fn buf() -> &'static mut [u8] {
    alloc::boxed::Box::leak(alloc::vec![0u8; 1024].into_boxed_slice())
}

/// Stand up one `embassy-net` stack over a driver-channel device, all `'static`.
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
    let resources = leak(embassy_net::StackResources::<4>::new());
    let (stack, net_runner) = embassy_net::new(device, config, resources, seed);
    (stack, net_runner, ch_runner)
}

/// Pump packets from one channel runner's TX into the other's RX, forever.
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

/// Run `foreground` to completion while the two crossover-wired `embassy-net`
/// stacks are polled in the background (which never ends on its own).
fn drive<Fut, F>(foreground: F)
where
    Fut: Future<Output = ()>,
    F: FnOnce(Stack<'static>, Stack<'static>) -> Fut,
{
    use core::future::poll_fn;
    use core::task::Poll;
    use std::time::{Duration, Instant};

    use futures::future::{join4, select, Either};
    use futures::pin_mut;

    // A stuck foreground (a transport regression or a dropped crossover packet)
    // would otherwise leave `block_on` pending forever, failing only at the outer
    // CI timeout. Bound every test with a wall-clock watchdog so `make check`
    // fails promptly and points at the hang instead.
    const WATCHDOG: Duration = Duration::from_secs(20);

    let (server_stack, mut server_net, server_ch) = make_stack(SERVER_IP, 0x1111_2222);
    let (client_stack, mut client_net, client_ch) = make_stack(CLIENT_IP, 0x3333_4444);

    let (server_state, server_rx, server_tx) = server_ch.split();
    let (client_state, client_rx, client_tx) = client_ch.split();
    server_state.set_link_state(LinkState::Up);
    client_state.set_link_state(LinkState::Up);

    let cable_s2c = cable(server_tx, client_rx);
    let cable_c2s = cable(client_tx, server_rx);

    // `run()` and the cables never return, so the join never completes — the
    // background is only ever cancelled by `select` when the foreground finishes.
    let background = join4(server_net.run(), client_net.run(), cable_s2c, cable_c2s);
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
                // Re-arm each poll so the executor keeps spinning and observes the
                // deadline even if the stacks and foreground would otherwise park.
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        });
        pin_mut!(watchdog);

        match select(session, watchdog).await {
            Either::Left((Either::Left(_), _)) => {}
            Either::Left((Either::Right(_), _)) => {
                panic!("background stacks ended before the test")
            }
            Either::Right(_) => panic!(
                "watchdog: foreground stuck for {}s (transport regression or dropped packet)",
                WATCHDOG.as_secs()
            ),
        }
    });
}

fn endpoint(port: u16) -> IpEndpoint {
    IpEndpoint::new(IpAddress::Ipv4(SERVER_IP), port)
}

/// Exchange one framed request + reply over an already-connected pair, asserting
/// the framing round-trips both ways.
async fn exchange(server: &mut dyn Connection, client: &mut dyn Connection, tag: &[u8]) {
    client.send(tag).await.expect("client send");
    let got = server
        .recv()
        .await
        .expect("server recv")
        .expect("server frame");
    assert_eq!(got, tag, "request framing round-trip");

    server.send(b"pong").await.expect("server send");
    let got = client
        .recv()
        .await
        .expect("client recv")
        .expect("client frame");
    assert_eq!(got, b"pong", "reply framing round-trip");
}

/// accept -> exchange -> disconnect -> socket recycled -> re-accept works.
#[test]
fn recycle_then_reaccept() {
    drive(|server_stack, client_stack| async move {
        let mut listener = TcpListener::new(server_stack, 7000u16, buf(), buf());
        let dialer = TcpDialer::new(client_stack, endpoint(7000), buf(), buf());

        // First connection over the single pooled socket.
        let (accepted, connected) = futures::join!(listener.accept(), dialer.connect());
        let mut server = accepted.expect("accept #1");
        let mut client = connected.expect("connect #1");
        exchange(server.as_mut(), client.as_mut(), b"one").await;

        // Disconnect: dropping both connections aborts and returns each socket to
        // its pool (the listener slot and the dialer slot).
        drop(server);
        drop(client);

        // Re-accept on the recycled listener socket; redial on the recycled dialer
        // socket. Both must succeed and carry a fresh framed exchange.
        let (accepted, connected) = futures::join!(listener.accept(), dialer.connect());
        let mut server = accepted.expect("accept #2 (recycled socket)");
        let mut client = connected.expect("connect #2 (recycled socket)");
        exchange(server.as_mut(), client.as_mut(), b"two").await;
    });
}

/// Two client/server sessions, each with its own accept slot, live at once — the
/// concurrent-accept-slots property the pool exists for. (The single-port
/// `TcpListener<N>` fan-out runs the AimX session engine, covered elsewhere.)
#[test]
fn two_concurrent_sessions() {
    drive(|server_stack, client_stack| async move {
        let mut listener_a = TcpListener::new(server_stack, 7001u16, buf(), buf());
        let mut listener_b = TcpListener::new(server_stack, 7002u16, buf(), buf());
        let dialer_a = TcpDialer::new(client_stack, endpoint(7001), buf(), buf());
        let dialer_b = TcpDialer::new(client_stack, endpoint(7002), buf(), buf());

        // Bring both sessions up concurrently, then exchange on both while both
        // are still open.
        let (a_srv, a_cli, b_srv, b_cli) = futures::join!(
            listener_a.accept(),
            dialer_a.connect(),
            listener_b.accept(),
            dialer_b.connect(),
        );
        let mut a_srv = a_srv.expect("accept A");
        let mut a_cli = a_cli.expect("connect A");
        let mut b_srv = b_srv.expect("accept B");
        let mut b_cli = b_cli.expect("connect B");

        futures::join!(
            exchange(a_srv.as_mut(), a_cli.as_mut(), b"aaa"),
            exchange(b_srv.as_mut(), b_cli.as_mut(), b"bbb"),
        );
    });
}

/// Dialer reuses its single socket: a connect to a port with no listener fails
/// (the peer stack RSTs), then a connect after a listener appears succeeds; and a
/// connect after the previous link was dropped succeeds again.
#[test]
fn dialer_redials_after_failure_and_drop() {
    drive(|server_stack, client_stack| async move {
        let dialer = TcpDialer::new(client_stack, endpoint(7003), buf(), buf());

        // No socket is listening on 7003 yet -> the server stack RSTs the SYN ->
        // connect fails. The dialer must recycle its socket for a redial.
        assert!(
            dialer.connect().await.is_err(),
            "connect should fail with no listener"
        );

        // Bring a listener up; the recycled dialer socket now connects.
        let mut listener = TcpListener::new(server_stack, 7003u16, buf(), buf());
        let (accepted, connected) = futures::join!(listener.accept(), dialer.connect());
        let mut server = accepted.expect("accept after listener up");
        let mut client = connected.expect("redial after failed connect");
        exchange(server.as_mut(), client.as_mut(), b"live").await;

        // Drop the link, then redial on the same dialer over its recycled socket.
        drop(server);
        drop(client);
        let (accepted, connected) = futures::join!(listener.accept(), dialer.connect());
        let mut server = accepted.expect("re-accept after drop");
        let mut client = connected.expect("redial after dropped link");
        exchange(server.as_mut(), client.as_mut(), b"again").await;
    });
}
