//! Host smoke for the Embassy broker session loop (`_test-embassy-broker`).
//!
//! The loop is what replaced mountain-mqtt-embassy's `run_with_subscriptions`
//! when the transport became injectable, so reconnect-and-resubscribe is this
//! crate's behaviour now rather than the helper's. Two `embassy-net` stacks
//! wired by an in-memory driver-channel crossover drive it against a fake
//! broker that speaks just enough MQTT: CONNECT/CONNACK, SUBSCRIBE/SUBACK, and
//! a server-initiated PUBLISH.
#![cfg(feature = "_test-embassy-broker")]

extern crate alloc;

use core::future::Future;

use embassy_net::{Config, Ipv4Address, Ipv4Cidr, Stack, StaticConfigV4};
use embassy_net_driver_channel as ch;
use embassy_net_driver_channel::driver::{HardwareAddress, LinkState};

// Each test binary defines these exactly once.
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
// No `defmt::timestamp!` here: this config enables the adapter's `embassy-time`,
// whose `defmt-timestamp-uptime` already defines `_defmt_timestamp`.

/// Real wall-clock time; a frozen `now()` stalls the stack's timers and the
/// session loop's reconnection delay.
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
const BROKER_IP: Ipv4Address = Ipv4Address::new(192, 168, 0, 1);
const CLIENT_IP: Ipv4Address = Ipv4Address::new(192, 168, 0, 2);
const BROKER_PORT: u16 = 1883;

type ChState = ch::State<MTU, 4, 4>;

fn leak<T>(v: T) -> &'static mut T {
    alloc::boxed::Box::leak(alloc::boxed::Box::new(v))
}

fn buf() -> &'static mut [u8] {
    alloc::boxed::Box::leak(alloc::vec![0u8; 2048].into_boxed_slice())
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
        dns_servers: Default::default(),
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

    let (broker_stack, mut broker_net, broker_ch) = make_stack(BROKER_IP, 0x1111_2222);
    let (client_stack, mut client_net, client_ch) = make_stack(CLIENT_IP, 0x3333_4444);

    let (broker_state, broker_rx, broker_tx) = broker_ch.split();
    let (client_state, client_rx, client_tx) = client_ch.split();
    broker_state.set_link_state(LinkState::Up);
    client_state.set_link_state(LinkState::Up);

    let background = join4(
        broker_net.run(),
        client_net.run(),
        cable(broker_tx, client_rx),
        cable(client_tx, broker_rx),
    );
    let foreground = foreground(broker_stack, client_stack);

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

// ---------------------------------------------------------------------------
// A fake broker: just enough MQTT 5 to complete a session.
// ---------------------------------------------------------------------------

/// Accept one TCP connection and answer CONNECT and SUBSCRIBE, then push a
/// PUBLISH. Records what it saw so the test can assert on the wire, not on
/// side effects.
#[derive(Default)]
struct Seen {
    connect: bool,
    subscribed_topics: alloc::vec::Vec<alloc::string::String>,
}

/// Read one MQTT packet: a fixed header byte, a varint remaining-length, then
/// that many bytes.
async fn read_packet(
    socket: &mut embassy_net::tcp::TcpSocket<'_>,
    buf: &mut alloc::vec::Vec<u8>,
) -> Option<(u8, alloc::vec::Vec<u8>)> {
    use embedded_io_async::Read;

    let mut byte = [0u8; 1];
    socket.read_exact(&mut byte).await.ok()?;
    let first = byte[0];

    let mut remaining = 0usize;
    let mut shift = 0;
    loop {
        socket.read_exact(&mut byte).await.ok()?;
        remaining |= ((byte[0] & 0x7F) as usize) << shift;
        if byte[0] & 0x80 == 0 {
            break;
        }
        shift += 7;
    }

    buf.clear();
    buf.resize(remaining, 0);
    socket.read_exact(buf).await.ok()?;
    Some((first, buf.clone()))
}

/// Encode a remaining-length varint.
fn varint(mut n: usize, out: &mut alloc::vec::Vec<u8>) {
    loop {
        let mut byte = (n % 128) as u8;
        n /= 128;
        if n > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if n == 0 {
            break;
        }
    }
}

async fn fake_broker(stack: Stack<'static>, seen: &core::cell::RefCell<Seen>) {
    use embedded_io_async::Write;

    let mut socket = embassy_net::tcp::TcpSocket::new(stack, buf(), buf());
    socket.set_timeout(None);
    if socket.accept(BROKER_PORT).await.is_err() {
        return;
    }

    let mut payload = alloc::vec::Vec::new();
    loop {
        let Some((first, body)) = read_packet(&mut socket, &mut payload).await else {
            return;
        };
        match first >> 4 {
            // CONNECT -> CONNACK (session present = 0, reason = success, no props)
            1 => {
                seen.borrow_mut().connect = true;
                let _ = socket.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).await;
            }
            // SUBSCRIBE -> SUBACK granting QoS 1 for each requested topic.
            8 => {
                // body: packet id (2) + property length (varint, 0 here) + payload
                let packet_id = [body[0], body[1]];
                let mut i = 2;
                // Skip the property length varint.
                while i < body.len() && body[i] & 0x80 != 0 {
                    i += 1;
                }
                i += 1;
                let mut granted = alloc::vec::Vec::new();
                while i + 2 <= body.len() {
                    let len = u16::from_be_bytes([body[i], body[i + 1]]) as usize;
                    i += 2;
                    if i + len > body.len() {
                        break;
                    }
                    seen.borrow_mut().subscribed_topics.push(
                        alloc::string::String::from_utf8_lossy(&body[i..i + len]).into_owned(),
                    );
                    i += len + 1; // topic + subscription options byte
                    granted.push(0x01);
                }
                let mut ack = alloc::vec::Vec::new();
                let mut rest = alloc::vec::Vec::new();
                rest.extend_from_slice(&packet_id);
                rest.push(0x00); // no properties
                rest.extend_from_slice(&granted);
                ack.push(0x90);
                varint(rest.len(), &mut ack);
                ack.extend_from_slice(&rest);
                let _ = socket.write_all(&ack).await;
            }
            // PINGREQ -> PINGRESP
            12 => {
                let _ = socket.write_all(&[0xD0, 0x00]).await;
            }
            // DISCONNECT
            14 => return,
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// The test.
// ---------------------------------------------------------------------------

/// The session loop completes a broker session over the injected transport:
/// CONNECT is answered, and the inbound topics are **subscribed on the wire**.
///
/// That subscribe is the property `run_with_subscriptions` used to provide and
/// this crate now owns — without it, inbound routing dies silently on the first
/// reconnect.
#[test]
fn the_session_loop_connects_and_subscribes() {
    use aimdb_core::buffer::BufferCfg;
    use aimdb_core::AimDbBuilder;
    use aimdb_mqtt_connector::MqttConnector;
    use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
    use alloc::sync::Arc;
    use core::cell::RefCell;

    let seen = RefCell::new(Seen::default());

    let outcome = drive(|broker_stack, client_stack| {
        let seen = &seen;
        async move {
            let stack: &'static Stack<'static> = leak(client_stack);

            let connector =
                MqttConnector::new(alloc::format!("mqtt://{}:{}", BROKER_IP, BROKER_PORT))
                    .transport(aimdb_embassy_adapter::net::EmbassyNet::tcp(
                        *stack,
                        buf(),
                        buf(),
                    ))
                    .with_client_id("host-smoke");

            let mut builder = AimDbBuilder::new()
                .runtime(Arc::new(TokioAdapter))
                .with_connector(connector);
            builder.configure::<u64>("temperature", |reg| {
                reg.buffer(BufferCfg::SingleLatest)
                    .link_from("mqtt://sensors/temperature")
                    .with_deserializer(|_ctx, data: &[u8]| {
                        core::str::from_utf8(data)
                            .ok()
                            .and_then(|s| s.trim().parse::<u64>().ok())
                            .ok_or_else(|| alloc::string::String::from("bad payload"))
                    })
                    .finish();
            });
            let (_db, runner) = builder.build().await.expect("build db");

            // Drive the runner (which owns the session loop) and the broker
            // together until the broker has seen a subscribe.
            let session = runner.run();
            let broker = fake_broker(broker_stack, seen);
            let until_subscribed = async {
                loop {
                    if !seen.borrow().subscribed_topics.is_empty() {
                        return;
                    }
                    embassy_time::Timer::after(embassy_time::Duration::from_millis(10)).await;
                }
            };

            futures::pin_mut!(session);
            futures::pin_mut!(broker);
            futures::pin_mut!(until_subscribed);
            let running = futures::future::select(session, broker);
            futures::pin_mut!(running);
            let _ = futures::future::select(running, until_subscribed).await;
        }
    });

    assert_eq!(outcome, Ok(()));
    let seen = seen.borrow();
    assert!(seen.connect, "the broker never saw a CONNECT");
    assert!(
        seen.subscribed_topics
            .iter()
            .any(|t| t == "sensors/temperature"),
        "the session must subscribe the inbound topic on the wire; saw {:?}",
        seen.subscribed_topics
    );
}
