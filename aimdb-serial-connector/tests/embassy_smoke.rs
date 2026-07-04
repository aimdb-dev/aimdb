//! Embassy client-exit smoke — the runtime-neutral `run_client` engine drives RPC
//! over the **real** Embassy serial transport ([`SerialDialer`] /
//! `EmbassySerialConnection`, COBS over `embedded-io-async`) on the
//! [`EmbassyAdapter`] clock. The `thumbv7em` monomorphization an MCU uses, driven
//! on the host by `futures::executor::block_on` (no `embassy-executor`, which does
//! not build on the host).
//!
//! Promotes Phase 5's stub-transport smoke
//! (`aimdb-embassy-adapter/tests/session_smoke.rs`) to the real serial transport:
//! a loopback UART carries the framed request back as its own reply (an
//! [`EchoCodec`], so no second node is needed), exercising COBS encode → wire →
//! decode under the engine.

#![cfg(feature = "embassy-runtime")]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::rc::Rc;
use core::cell::RefCell;
use core::future::poll_fn;
use core::task::{Poll, Waker};
use std::sync::Arc;

use embedded_io_async::{ErrorKind, ErrorType, Read, Write};

use aimdb_core::session::{
    run_client, ClientConfig, CodecError, EnvelopeCodec, Inbound, Outbound, Payload,
};
use aimdb_embassy_adapter::connectors::{EmbassyConnection, OneShotDialer};
use aimdb_embassy_adapter::EmbassyAdapter;
use aimdb_serial_connector::embassy_transport::CobsFramer;

// No-op defmt logger + host time driver so the binary links: the engine holds
// the adapter as `Arc<dyn RuntimeOps>`, whose vtable references
// the `log` path (`defmt` on Embassy) even though this smoke never logs.
aimdb_embassy_adapter::host_test_stubs!();

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
            Inbound::Request { id, params, .. } => {
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

/// A single-threaded async byte loopback: bytes written to the shared queue become
/// readable from the same handle. Two clones (one as `rx`, one as `tx`) form the
/// UART halves of a self-replying serial port.
#[derive(Clone, Default)]
struct LoopbackUart {
    shared: Rc<RefCell<Shared>>,
}

#[derive(Default)]
struct Shared {
    buf: VecDeque<u8>,
    reader_waker: Option<Waker>,
}

impl ErrorType for LoopbackUart {
    type Error = ErrorKind;
}

impl Write for LoopbackUart {
    async fn write(&mut self, data: &[u8]) -> Result<usize, Self::Error> {
        let mut s = self.shared.borrow_mut();
        s.buf.extend(data.iter().copied());
        if let Some(w) = s.reader_waker.take() {
            w.wake();
        }
        Ok(data.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(()) // in-memory loopback: writes are immediately visible
    }
}

impl Read for LoopbackUart {
    async fn read(&mut self, out: &mut [u8]) -> Result<usize, Self::Error> {
        poll_fn(|cx| {
            let mut s = self.shared.borrow_mut();
            if s.buf.is_empty() {
                s.reader_waker = Some(cx.waker().clone());
                return Poll::Pending;
            }
            let n = out.len().min(s.buf.len());
            for slot in out.iter_mut().take(n) {
                *slot = s.buf.pop_front().unwrap();
            }
            Poll::Ready(Ok(n))
        })
        .await
    }
}

#[test]
fn embassy_clock_drives_client_engine_rpc_over_serial() {
    use futures::executor::block_on;
    use futures::future::{select, Either};

    // The exact `run_client<SerialDialer<_, _>, _, EmbassyAdapter>` monomorphization
    // an MCU build uses — over the real COBS serial connection.
    let clock = Arc::new(EmbassyAdapter::default());
    let config = ClientConfig {
        reconnect: false,
        sends_hello: false,
        ..ClientConfig::default()
    };

    let uart = LoopbackUart::default();
    // The one-shot dialer over the real COBS framed connection — the exact spine
    // an MCU build uses (`OneShotDialer<EmbassyConnection<_, _, CobsFramer>>`).
    let conn = EmbassyConnection::<_, _, _>::new(uart.clone(), uart, CobsFramer::new());
    let dialer = OneShotDialer::new(conn);
    let (handle, engine_fut) = run_client(dialer, EchoCodec, config, clock);

    block_on(async move {
        futures::pin_mut!(engine_fut);
        let call = handle.call("echo", Payload::from(&b"ping"[..]));
        futures::pin_mut!(call);

        // Drive the engine concurrently with the call; the reply must arrive (the
        // framed request, COBS round-tripped through the loopback) before the engine
        // ends.
        match select(call, engine_fut).await {
            Either::Left((reply, _engine)) => {
                assert_eq!(&*reply.expect("call should resolve"), b"ping");
            }
            Either::Right(_) => panic!("engine ended before the reply arrived"),
        }
    });
}
