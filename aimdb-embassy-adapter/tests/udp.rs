//! Host smoke for the Embassy [`Datagram`] path over two crossover-wired
//! `embassy-net` stacks.
//!
//! Covers what a KNX/IP tunnel needs from it: a real bound address to advertise
//! (`local_addr`), a round trip carrying the sender's address, and rebinding the
//! same socket across a reconnect cycle.
#![cfg(feature = "net")]

extern crate alloc;

use core::future::Future;

use aimdb_core::session::{Datagram, DatagramBinder};
use aimdb_embassy_adapter::net::EmbassyNet;
use embassy_net::udp::PacketMetadata;
use embassy_net::{Config, Ipv4Address, Ipv4Cidr, Stack, StaticConfigV4};
use embassy_net_driver_channel as ch;
use embassy_net_driver_channel::driver::{HardwareAddress, LinkState};

// Each test binary must define these exactly once.
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

/// Real wall-clock time; a frozen `now()` stalls the stack's timers.
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
const A_IP: Ipv4Address = Ipv4Address::new(192, 168, 0, 1);
const B_IP: Ipv4Address = Ipv4Address::new(192, 168, 0, 2);

type ChState = ch::State<MTU, 4, 4>;

fn leak<T>(v: T) -> &'static mut T {
    alloc::boxed::Box::leak(alloc::boxed::Box::new(v))
}

fn buf() -> &'static mut [u8] {
    alloc::boxed::Box::leak(alloc::vec![0u8; 1024].into_boxed_slice())
}

fn meta() -> &'static mut [PacketMetadata] {
    alloc::boxed::Box::leak(alloc::vec![PacketMetadata::EMPTY; 8].into_boxed_slice())
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
    let resources = leak(embassy_net::StackResources::<4>::new());
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

/// Run `foreground` while both stacks poll in the background, watchdogged so a
/// hang fails the test rather than the CI job.
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

    const WATCHDOG: Duration = Duration::from_secs(20);

    let (a_stack, mut a_net, a_ch) = make_stack(A_IP, 0x1111_2222);
    let (b_stack, mut b_net, b_ch) = make_stack(B_IP, 0x3333_4444);

    let (a_state, a_rx, a_tx) = a_ch.split();
    let (b_state, b_rx, b_tx) = b_ch.split();
    a_state.set_link_state(LinkState::Up);
    b_state.set_link_state(LinkState::Up);

    let background = join4(
        a_net.run(),
        b_net.run(),
        cable(a_tx, b_rx),
        cable(b_tx, a_rx),
    );
    let foreground = foreground(a_stack, b_stack);

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

/// A datagram round trip, with both ends reporting a routable bound address —
/// what a tunnel handshake advertises instead of `0.0.0.0:0`.
#[test]
fn udp_round_trips_and_reports_a_real_bound_address() {
    let outcome = drive(|a_stack, b_stack| async move {
        let a_binder = EmbassyNet::udp(a_stack, meta(), buf(), meta(), buf());
        let b_binder = EmbassyNet::udp(b_stack, meta(), buf(), meta(), buf());

        let mut a = a_binder.bind(3671).await.expect("bind A");
        let mut b = b_binder.bind(3672).await.expect("bind B");

        let a_addr = a.local_addr().expect("A must report a bound address");
        let b_addr = b.local_addr().expect("B must report a bound address");
        assert_eq!(a_addr, core::net::SocketAddr::new(A_IP.into(), 3671));
        assert_eq!(b_addr, core::net::SocketAddr::new(B_IP.into(), 3672));

        a.send_to(b"tunnel", b_addr).await.expect("A send");

        let mut got = [0u8; 32];
        let (n, from) = b.recv_from(&mut got).await.expect("B recv");
        assert_eq!(&got[..n], b"tunnel");
        assert_eq!(from, a_addr, "source address must be the sender's");
    });
    assert_eq!(outcome, Ok(()));
}

/// A socket reset drops the socket and binds a fresh one on the same binder —
/// the cycle `Action::ResetSocket` drives. The buffers are reused, so the
/// rebound socket must still work.
#[test]
fn a_binder_rebinds_after_its_socket_is_dropped() {
    let outcome = drive(|a_stack, b_stack| async move {
        let a_binder = EmbassyNet::udp(a_stack, meta(), buf(), meta(), buf());
        let b_binder = EmbassyNet::udp(b_stack, meta(), buf(), meta(), buf());
        let mut b = b_binder.bind(3672).await.expect("bind B");
        let b_addr = b.local_addr().expect("B bound address");

        let first = a_binder.bind(3671).await.expect("first bind");
        drop(first);

        let mut second = a_binder.bind(3673).await.expect("rebind on a new port");
        assert_eq!(second.local_addr().unwrap().port(), 3673);

        second.send_to(b"after-reset", b_addr).await.expect("send");
        let mut got = [0u8; 32];
        let (n, from) = b.recv_from(&mut got).await.expect("recv");
        assert_eq!(&got[..n], b"after-reset");
        assert_eq!(from.port(), 3673, "the rebound port must be on the wire");
    });
    assert_eq!(outcome, Ok(()));
}

/// The binder owns exactly one socket, so a second bind while the first is
/// live is refused rather than silently sharing.
#[test]
fn a_second_bind_fails_while_the_socket_is_held() {
    let outcome = drive(|a_stack, _b_stack| async move {
        let binder = EmbassyNet::udp(a_stack, meta(), buf(), meta(), buf());
        let _held = binder.bind(3671).await.expect("first bind");
        assert!(binder.bind(3672).await.is_err(), "socket is already taken");
    });
    assert_eq!(outcome, Ok(()));
}
