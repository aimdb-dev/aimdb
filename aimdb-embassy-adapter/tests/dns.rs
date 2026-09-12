//! Host smoke for the resolution half of [`StreamDialer`] on Embassy.
//!
//! `connect` takes a host *string* and every adapter must accept both a
//! hostname and an IP literal, or a connector has to grow a per-runtime
//! validation gate — which is exactly what `mqtt://`/`mqtts://` had. Only a
//! real stack can show a name being queried, so two crossover-wired
//! `embassy-net` stacks drive it: B answers DNS on UDP/53 and listens on TCP,
//! A dials it by name.
#![cfg(feature = "net")]

extern crate alloc;

use core::future::Future;

use aimdb_core::session::{
    ByteStream, Datagram, DatagramBinder, StreamDialer, StreamListener, TransportError,
};
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

/// Real wall-clock time; a frozen `now()` stalls the stack's timers, and DNS
/// retransmission is on one of them.
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
const PORT: u16 = 7301;
const DNS_PORT: u16 = 53;

/// The only name the stub resolver knows, pointing at B.
const BROKER: &str = "broker.test";

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
    dns: Option<Ipv4Address>,
    seed: u64,
) -> (
    Stack<'static>,
    embassy_net::Runner<'static, ch::Device<'static, MTU>>,
    ch::Runner<'static, MTU>,
) {
    let state: &'static mut ChState = leak(ch::State::new());
    let (ch_runner, device) = ch::new(state, HardwareAddress::Ip);
    let mut dns_servers = heapless::Vec::new();
    if let Some(server) = dns {
        dns_servers.push(server).expect("one DNS server fits");
    }
    let config = Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(ip, 24),
        gateway: None,
        dns_servers,
    });
    // One slot over the three sockets the test opens: `embassy_net::new` adds
    // the resolver socket itself now that `net` enables `embassy-net/dns`.
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

// ===========================================================================
// Stub resolver.
// ===========================================================================

/// Build a reply to `query`: one `A` record holding [`B_IP`] when the query
/// names [`BROKER`], `NXDomain` otherwise.
///
/// Enough of RFC 1035 to satisfy smoltcp's client and no more — it checks the
/// transaction id, the question type, and that the answer's name equals the
/// one it asked about, so the question is echoed verbatim and the answer name
/// repeated uncompressed rather than written as a `0xC00C` pointer.
fn reply(query: &[u8]) -> Option<alloc::vec::Vec<u8>> {
    const A: u16 = 0x0001;
    const IN: u16 = 0x0001;

    if query.len() < 12 {
        return None;
    }
    // Walk the QNAME's length-prefixed labels to the root label. A query never
    // uses compression, so every octet here is a length.
    let mut root = 12;
    while *query.get(root)? != 0 {
        root += 1 + *query.get(root)? as usize;
    }
    let name = query.get(12..=root)?;
    let qtype = u16::from_be_bytes([*query.get(root + 1)?, *query.get(root + 2)?]);
    let question_end = root + 5;
    let asked = name_to_str(name);

    let mut reply = query.get(..question_end)?.to_vec();
    let known = asked == BROKER && qtype == A;
    // QR | recursion desired | recursion available, plus NXDomain (rcode 3)
    // for anything but the one name-and-type the stub serves. A name it knows
    // and a type it does not gets NOERROR with no answer, as a real resolver
    // would for an `AAAA` on a v4-only host.
    let flags: u16 = if asked == BROKER { 0x8180 } else { 0x8183 };
    reply[2..4].copy_from_slice(&flags.to_be_bytes());
    reply[6..8].copy_from_slice(&u16::from(known).to_be_bytes());
    if known {
        reply.extend_from_slice(name);
        reply.extend_from_slice(&A.to_be_bytes());
        reply.extend_from_slice(&IN.to_be_bytes());
        reply.extend_from_slice(&60u32.to_be_bytes()); // TTL
        reply.extend_from_slice(&4u16.to_be_bytes()); // RDLENGTH
        reply.extend_from_slice(&B_IP.octets());
    }
    Some(reply)
}

/// Render a wire-format QNAME as `label.label`, for comparison against
/// [`BROKER`].
fn name_to_str(name: &[u8]) -> alloc::string::String {
    let mut out = alloc::string::String::new();
    let mut i = 0;
    while let Some(&len) = name.get(i) {
        if len == 0 {
            break;
        }
        let Some(label) = name.get(i + 1..i + 1 + len as usize) else {
            break;
        };
        if !out.is_empty() {
            out.push('.');
        }
        out.push_str(&alloc::string::String::from_utf8_lossy(label));
        i += 1 + len as usize;
    }
    out
}

/// Answer queries on `stack`'s UDP/53 forever.
async fn serve_dns(stack: Stack<'static>) -> ! {
    let mut socket = EmbassyNet::udp(stack, meta(), buf(), meta(), buf())
        .bind(DNS_PORT)
        .await
        .expect("bind the stub resolver");
    let mut rx = [0u8; 512];
    loop {
        let (len, from) = socket.recv_from(&mut rx).await.expect("read a query");
        if let Some(reply) = reply(&rx[..len]) {
            socket.send_to(&reply, from).await.expect("write a reply");
        }
    }
}

// ===========================================================================
// Rig.
// ===========================================================================

/// Run `foreground` while both stacks poll and B resolves in the background,
/// watchdogged so a hang fails the test rather than the CI job.
fn drive<Fut, F>(foreground: F) -> Result<(), &'static str>
where
    Fut: Future<Output = ()>,
    F: FnOnce(Stack<'static>, Stack<'static>) -> Fut,
{
    use core::future::poll_fn;
    use core::task::Poll;
    use std::time::{Duration, Instant};

    use futures::future::{join, join4, select, Either};
    use futures::pin_mut;

    const WATCHDOG: Duration = Duration::from_secs(20);

    let (a_stack, mut a_net, a_ch) = make_stack(A_IP, Some(B_IP), 0x1111_2222);
    let (b_stack, mut b_net, b_ch) = make_stack(B_IP, None, 0x3333_4444);

    let (a_state, a_rx, a_tx) = a_ch.split();
    let (b_state, b_rx, b_tx) = b_ch.split();
    a_state.set_link_state(LinkState::Up);
    b_state.set_link_state(LinkState::Up);

    let background = join(
        join4(
            a_net.run(),
            b_net.run(),
            cable(a_tx, b_rx),
            cable(b_tx, a_rx),
        ),
        serve_dns(b_stack),
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

// ===========================================================================
// Tests.
// ===========================================================================

/// The regression behind `mqtts://broker.example.com`: a hostname reached the
/// dialer, which only parsed IP literals, so the connector reconnect-looped
/// forever. Dial by name and exchange a byte to prove the resolved address is
/// the one that got connected.
#[test]
fn dials_a_hostname() {
    let outcome = drive(|a_stack, b_stack| async move {
        use futures::future::join;

        let dialer = EmbassyNet::tcp(a_stack, buf(), buf());
        let mut listener = EmbassyNet::listen::<1>(b_stack, PORT, [(buf(), buf())]);

        let (dialed, accepted) = join(dialer.connect(BROKER, PORT), listener.accept()).await;
        let mut client = dialed.expect("a hostname must dial");
        let (mut server, _peer) = accepted.expect("accept");

        client.write_all(b"ping").await.expect("write");
        client.flush().await.expect("flush");
        let mut got = [0u8; 4];
        server.read(&mut got).await.expect("read");
        assert_eq!(&got, b"ping");
    });
    assert_eq!(outcome, Ok(()));
}

/// An IP literal still dials without a query, so a deployment with no resolver
/// configured is unaffected by the name path above.
#[test]
fn dials_an_ip_literal() {
    let outcome = drive(|a_stack, b_stack| async move {
        use futures::future::join;

        let dialer = EmbassyNet::tcp(a_stack, buf(), buf());
        let mut listener = EmbassyNet::listen::<1>(b_stack, PORT, [(buf(), buf())]);

        let (dialed, accepted) = join(dialer.connect("192.168.0.2", PORT), listener.accept()).await;
        dialed.expect("an IP literal must dial");
        accepted.expect("accept");
    });
    assert_eq!(outcome, Ok(()));
}

/// A name that does not resolve fails as a connect failure would, and — the
/// part that matters for a reconnect loop — hands the socket back, so the next
/// dial is not stuck on [`TransportError::Busy`] forever.
#[test]
fn an_unresolvable_name_fails_and_frees_the_socket() {
    let outcome = drive(|a_stack, b_stack| async move {
        use futures::future::join;

        let dialer = EmbassyNet::tcp(a_stack, buf(), buf());
        let mut listener = EmbassyNet::listen::<1>(b_stack, PORT, [(buf(), buf())]);

        assert_eq!(
            dialer.connect("nowhere.test", PORT).await.err(),
            Some(TransportError::Io),
            "an unknown name is an I/O failure, not a panic or a hang"
        );

        let (dialed, accepted) = join(dialer.connect(BROKER, PORT), listener.accept()).await;
        dialed.expect("the socket must still be dialable");
        accepted.expect("accept");
    });
    assert_eq!(outcome, Ok(()));
}
