//! Phase 5 Embassy smoke — the runtime-neutral session **client engine** runs on
//! the Embassy adapter's [`TimeOps`](aimdb_executor::TimeOps) clock.
//!
//! `run_client` is parametrized over the runtime clock (its only runtime
//! dependency, for reconnect backoff / keepalive). This test instantiates it with
//! [`EmbassyAdapter`] — the same monomorphization an MCU build uses — and drives
//! the returned spawn-free engine future over a stub loopback transport,
//! round-tripping one record (an RPC `call` whose reply echoes the params). It
//! validates the Embassy seam (engine future + `EmbassyAdapter::sleep` clock)
//! without needing `embassy-executor`, which does not build on the host; the
//! spawn-free `BoxFut` is driven by `futures::executor::block_on`.
//!
//! Runs under the same host feature set as the other embassy-adapter host tests
//! (`alloc,embassy-sync,embassy-time`); `connector-session` is pulled in for the
//! engine via this crate's dev-dependency on `aimdb-core`.

#![cfg(feature = "embassy-time")]

use std::sync::Arc;

use aimdb_core::session::{
    run_client, BoxFut, ClientConfig, CodecError, Connection, Dialer, EnvelopeCodec, Inbound,
    Outbound, Payload, PeerInfo, TransportError, TransportResult,
};
use aimdb_embassy_adapter::EmbassyAdapter;

// Trivial host time driver so `embassy_time` links (the happy path never awaits
// `clock.sleep`, so `now`/`schedule_wake` are never actually exercised).
struct TestTimeDriver;
impl embassy_time_driver::Driver for TestTimeDriver {
    fn now(&self) -> u64 {
        0
    }
    fn schedule_wake(&self, _at: u64, _waker: &core::task::Waker) {}
}
embassy_time_driver::time_driver_impl!(static TEST_TIME_DRIVER: TestTimeDriver = TestTimeDriver);

/// Minimal echo wire: a `Request` is `[id:8][params]`; the loopback returns those
/// bytes verbatim, which `decode_outbound` reads back as `Reply { id, Ok(params) }`.
struct EchoCodec;

impl EnvelopeCodec for EchoCodec {
    fn decode(&self, _frame: &[u8]) -> Result<Inbound, CodecError> {
        Err(CodecError::Malformed) // server direction unused by this client smoke
    }
    fn encode(&self, _msg: Outbound<'_>, _out: &mut Vec<u8>) -> Result<(), CodecError> {
        Err(CodecError::Malformed)
    }
    fn encode_inbound(&self, msg: Inbound, out: &mut Vec<u8>) -> Result<(), CodecError> {
        match msg {
            Inbound::Request {
                id,
                method: _,
                params,
            } => {
                out.extend_from_slice(&id.to_be_bytes());
                out.extend_from_slice(&params);
                Ok(())
            }
            _ => Err(CodecError::Malformed),
        }
    }
    fn decode_outbound<'a>(&self, frame: &'a [u8]) -> Result<Outbound<'a>, CodecError> {
        if frame.len() < 8 {
            return Err(CodecError::Malformed);
        }
        let id = u64::from_be_bytes(frame[0..8].try_into().unwrap());
        Ok(Outbound::Reply {
            id,
            result: Ok(Payload::from(&frame[8..])),
        })
    }
}

/// A loopback connection: every `send` echoes the frame straight back to `recv`.
struct Loopback {
    tx: futures::channel::mpsc::UnboundedSender<Vec<u8>>,
    rx: futures::channel::mpsc::UnboundedReceiver<Vec<u8>>,
    peer: PeerInfo,
}

impl Connection for Loopback {
    fn recv(&mut self) -> BoxFut<'_, TransportResult<Option<Vec<u8>>>> {
        Box::pin(async move {
            use futures::StreamExt;
            Ok(self.rx.next().await) // `None` once every sender drops
        })
    }
    fn send<'a>(&'a mut self, frame: &'a [u8]) -> BoxFut<'a, TransportResult<()>> {
        let tx = self.tx.clone();
        let bytes = frame.to_vec();
        Box::pin(async move { tx.unbounded_send(bytes).map_err(|_| TransportError::Closed) })
    }
    fn peer(&self) -> &PeerInfo {
        &self.peer
    }
}

/// Dials a fresh loopback connection.
struct StubDialer;

impl Dialer for StubDialer {
    fn connect(&self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
        Box::pin(async {
            let (tx, rx) = futures::channel::mpsc::unbounded();
            Ok(Box::new(Loopback {
                tx,
                rx,
                peer: PeerInfo::default(),
            }) as Box<dyn Connection>)
        })
    }
}

#[test]
fn embassy_clock_drives_client_engine_rpc() {
    use futures::executor::block_on;
    use futures::future::{select, Either};

    // The exact `run_client<_, _, EmbassyAdapter>` monomorphization an MCU uses.
    let clock = Arc::new(EmbassyAdapter::default());
    let config = ClientConfig {
        reconnect: false,
        sends_hello: false,
        ..ClientConfig::default()
    };
    let (handle, engine_fut) = run_client(StubDialer, EchoCodec, config, clock);

    block_on(async move {
        futures::pin_mut!(engine_fut);
        let call = handle.call("echo", Payload::from(&b"ping"[..]));
        futures::pin_mut!(call);

        // Drive the engine concurrently with the call; the engine must reach the
        // reply, not end first.
        match select(call, engine_fut).await {
            Either::Left((reply, _engine)) => {
                assert_eq!(&*reply.expect("call should resolve"), b"ping");
            }
            Either::Right(_) => panic!("engine ended before the reply arrived"),
        }
    });
}
