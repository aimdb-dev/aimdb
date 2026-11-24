use super::*;
use crate::session::{CodecError, PeerInfo, TransportResult};
use core::future::pending;

/// Sink connection: swallows every frame and never yields one, i.e. a peer
/// that keeps the link open and answers nothing.
#[derive(Default)]
struct SilentPeer {
    peer: PeerInfo,
}

impl Connection for SilentPeer {
    fn recv(&mut self) -> BoxFut<'_, TransportResult<Option<Vec<u8>>>> {
        Box::pin(pending())
    }
    fn send<'a>(&'a mut self, _frame: &'a [u8]) -> BoxFut<'a, TransportResult<()>> {
        Box::pin(async { Ok(()) })
    }
    fn peer(&self) -> &PeerInfo {
        &self.peer
    }
}

/// Frames are never inspected in these tests — only the pending-call
/// bookkeeping is.
struct NullCodec;

impl EnvelopeCodec for NullCodec {
    fn decode(&self, _frame: &[u8]) -> Result<Inbound, CodecError> {
        Err(CodecError::Malformed)
    }
    fn encode(&self, _msg: Outbound<'_>, _out: &mut Vec<u8>) -> Result<(), CodecError> {
        Ok(())
    }
    fn encode_inbound(&self, _msg: Inbound, _out: &mut Vec<u8>) -> Result<(), CodecError> {
        Ok(())
    }
    fn decode_outbound<'a>(&self, _frame: &'a [u8]) -> Result<Outbound<'a>, CodecError> {
        Err(CodecError::Malformed)
    }
}

/// A handle whose channel nobody drains — an engine still dialing, or
/// sleeping between redials. `cap` is [`ClientConfig::max_offline_queue`].
/// The prune receiver comes back too: dropping it would close the signal
/// channel and silently no-op every [`ClientHandle::request_prune`].
fn test_handle_capped(cap: usize) -> (ClientHandle, Receiver<ClientCmd>, Receiver<()>) {
    let (cmd_tx, cmd_rx) = async_channel::bounded(cap.max(1));
    let (prune_tx, prune_rx) = async_channel::bounded(1);
    (
        ClientHandle {
            cmd_tx,
            prune_tx,
            next_id: Arc::new(AtomicUsize::new(1)),
            inline_envelopes: true,
        },
        cmd_rx,
        prune_rx,
    )
}

fn test_handle() -> (ClientHandle, Receiver<ClientCmd>, Receiver<()>) {
    test_handle_capped(256)
}

/// Poll a call once — enough to queue its command — then abandon it, which
/// is what a lost timeout race does to the future.
async fn abandon_call(handle: &ClientHandle, method: &'static str) {
    let call = handle.call(method, Payload::from(&b"x"[..]));
    assert!(
        tokio::time::timeout(core::time::Duration::ZERO, call)
            .await
            .is_err(),
        "the call must not resolve"
    );
}

/// The caller half of the reclaim path: abandoning a call queues a
/// `CancelCall` naming exactly that call, so the engine never has to guess
/// which entry died.
#[tokio::test]
async fn abandoning_a_call_queues_its_cancellation() {
    let (handle, cmd_rx, _prune_rx) = test_handle();
    abandon_call(&handle, "one").await;

    match cmd_rx.try_recv() {
        Ok(ClientCmd::Call { id: 1, .. }) => {}
        _ => panic!("the call must be queued first"),
    }
    match cmd_rx.try_recv() {
        Ok(ClientCmd::CancelCall { id: 1 }) => {}
        _ => panic!("abandoning the call must queue its cancellation"),
    }
    assert!(cmd_rx.is_empty(), "no other command is emitted");
}

/// A call that resolves normally must not also queue a cancellation — the
/// engine already dropped its entry when it routed the reply.
#[tokio::test]
async fn a_resolved_call_queues_no_cancellation() {
    let (handle, cmd_rx, _prune_rx) = test_handle();
    let call = handle.call("one", Payload::from(&b"x"[..]));
    let answer = async {
        // Take the reply channel out of the queued command and answer it.
        match cmd_rx.recv().await {
            Ok(ClientCmd::Call { reply, .. }) => {
                let _ = reply.send(Ok(Payload::from(&b"ok"[..])));
            }
            _ => panic!("the call must be queued"),
        }
    };
    let (reply, ()) = futures_util::future::join(call, answer).await;
    assert_eq!(&*reply.expect("the call resolves"), b"ok");
    assert!(
        cmd_rx.is_empty(),
        "a resolved call must not queue a cancellation"
    );
}

/// Regression for the *concurrent* timeout batch: every call is issued and
/// tracked while its receiver is live, then all of them are abandoned at
/// once, with keepalive disabled and no later call to trigger a sweep.
///
/// Each entry the engine frees drops its reply sender, which the (retained)
/// receiver observes as `Canceled` — so this asserts the table actually
/// empties rather than that some reclaim path merely ran. Pre-fix all 1000
/// entries stayed until the connection ended.
#[tokio::test]
async fn a_concurrent_batch_of_cancellations_frees_every_entry() {
    const BATCH: u64 = 1_000;
    let (cmd_tx, cmd_rx) = async_channel::unbounded::<ClientCmd>();
    let config = ClientConfig {
        reconnect: false,
        // The point of the test: nothing but the cancellations can reclaim.
        keepalive_interval: None,
        ..Default::default()
    };
    let ctx = crate::RuntimeContext::new(Arc::new(crate::executor::test_support::NoopRuntimeOps));
    // Never signalled: this asserts the by-id path reclaims on its own.
    let (_prune_tx, prune_rx) = async_channel::bounded::<()>(1);
    let engine = drive_connection(
        Box::new(SilentPeer::default()),
        &NullCodec,
        &cmd_rx,
        &prune_rx,
        &config,
        &ctx,
    );

    let exercise = async {
        // All in flight at once: every receiver is alive as its call is
        // tracked, so nothing is reclaimable until the batch is abandoned.
        let mut replies = Vec::new();
        for id in 1..=BATCH {
            let (reply, rx) = oneshot::channel();
            cmd_tx
                .try_send(ClientCmd::Call {
                    id,
                    method: String::from("hang"),
                    params: Payload::from(&b"x"[..]),
                    reply,
                })
                .expect("the channel is unbounded");
            replies.push(rx);
        }
        // Then the whole batch times out.
        for id in 1..=BATCH {
            cmd_tx
                .try_send(ClientCmd::CancelCall { id })
                .expect("the channel is unbounded");
        }
        for (i, rx) in replies.into_iter().enumerate() {
            assert!(
                rx.await.is_err(),
                "call {} was still held by the engine",
                i + 1
            );
        }
    };

    futures_util::pin_mut!(engine);
    futures_util::pin_mut!(exercise);
    match futures_util::future::select(engine, exercise).await {
        futures_util::future::Either::Left(_) => panic!("the engine ended early"),
        futures_util::future::Either::Right(((), _)) => {}
    }
}

/// The id counter refuses rather than wraps at its ceiling: a wrapped id
/// would re-issue one a live subscription still holds, and the engine would
/// demux that subscription's updates into the wrong sink. Both allocating
/// call sites fail, and neither reaches the engine — so no pending entry is
/// ever created under an id that would be mis-routed.
#[tokio::test]
async fn exhausted_ids_are_refused_not_wrapped() {
    let (cmd_tx, cmd_rx) = async_channel::bounded(256);
    let (prune_tx, _prune_rx) = async_channel::bounded(1);
    let handle = ClientHandle {
        cmd_tx,
        prune_tx,
        next_id: Arc::new(AtomicUsize::new(usize::MAX)),
        inline_envelopes: true,
    };

    // Bounded at zero: the refusal happens before the id is queued, so it
    // resolves on the first poll. A wrapping counter would instead queue the
    // call and await a reply forever — the timeout turns that regression
    // into a failure rather than a hang.
    let refused = tokio::time::timeout(
        core::time::Duration::ZERO,
        handle.call("one", Payload::from(&b"x"[..])),
    )
    .await
    .expect("a refused call must not reach the engine and await a reply");
    assert!(
        matches!(refused, Err(RpcError::Internal)),
        "a call at the id ceiling must be refused"
    );
    assert!(
        handle.subscribe("tele").is_err(),
        "a subscription at the id ceiling must be refused"
    );
    assert!(
        cmd_rx.is_empty(),
        "a refused allocation must not reach the engine"
    );
}

/// Evict-oldest puts a cancellation on the same lossy footing as a write,
/// but dropping one silently strands its pending entry until the connection
/// ends — so the eviction has to be noticed. Nothing drains the queue here:
/// the saturated state a pending dial or a backoff sleep produces.
#[tokio::test]
async fn an_evicted_cancellation_raises_the_prune_signal() {
    let (handle, cmd_rx, prune_rx) = test_handle_capped(1);

    handle
        .enqueue(ClientCmd::CancelCall { id: 1 })
        .expect("the engine is still running");
    assert!(prune_rx.is_empty(), "nothing has been evicted yet");

    // Fills the single slot, displacing the cancellation.
    handle
        .write("tele", Payload::from(&b"x"[..]))
        .expect("the engine is still running");

    assert_eq!(cmd_rx.len(), 1, "the cap still holds");
    assert_eq!(
        prune_rx.len(),
        1,
        "an evicted cancellation must fall back to a sweep"
    );
}

/// The negative control: eviction is the documented, caller-visible cost of
/// the bound for ordinary commands. Signalling on every one would drag an
/// `O(N)` sweep into steady-state backpressure.
#[tokio::test]
async fn an_evicted_write_raises_no_prune_signal() {
    let (handle, _cmd_rx, prune_rx) = test_handle_capped(1);

    for topic in ["one", "two"] {
        handle
            .write(topic, Payload::from(&b"x"[..]))
            .expect("the engine is still running");
    }

    assert!(
        prune_rx.is_empty(),
        "an evicted write is expected loss, not a leak"
    );
}

/// A cancellation is bookkeeping; a queued `Write` is work the caller asked
/// for. Cap 2, so the write and the call itself both fit and the
/// cancellation is the only command that finds the queue full.
#[tokio::test]
async fn a_cancellation_never_evicts_a_queued_write() {
    let (handle, cmd_rx, prune_rx) = test_handle_capped(2);

    handle
        .write("tele", Payload::from(&b"x"[..]))
        .expect("the engine is still running");
    abandon_call(&handle, "hang").await;

    assert_eq!(cmd_rx.len(), 2, "the cap still holds");
    match cmd_rx.try_recv() {
        Ok(ClientCmd::Write { topic, .. }) => assert_eq!(topic, "tele"),
        _ => panic!("the caller's write must survive the cancellation"),
    }
    match cmd_rx.try_recv() {
        Ok(ClientCmd::Call { id: 1, .. }) => {}
        _ => panic!("the call itself must still be queued"),
    }
    assert_eq!(
        prune_rx.len(),
        1,
        "the cancellation it could not queue must fall back to a sweep"
    );
}

/// The reclaim itself: an abandoned entry goes, a live one stays. Identity
/// is the dropped receiver, which is why the sweep can stand in for a
/// cancellation that never arrived.
#[test]
fn the_sweep_frees_abandoned_entries_and_keeps_live_ones() {
    let mut pending: PendingCalls = HashMap::new();
    let (abandoned, rx) = oneshot::channel();
    let (live, _live_rx) = oneshot::channel();
    pending.insert(1, abandoned);
    pending.insert(2, live);
    drop(rx); // the caller's future went away

    sweep_abandoned(&mut pending);

    assert!(
        !pending.contains_key(&1),
        "the abandoned call must be freed"
    );
    assert!(
        pending.contains_key(&2),
        "a caller still awaiting its reply must not be dropped"
    );
}

/// End to end at capacity 1 against a peer that answers nothing: the exact
/// loss the bound can inflict. The engine files a call while its caller
/// still waits, the caller then abandons it — the cancellation lands in the
/// drained queue — and a write evicts that cancellation. The engine still
/// reclaims: keepalive off and no later command, so the signal is the only
/// thing that can wake the sweep.
///
/// Consuming the signal *is* the sweep running (one match arm), and
/// [`the_sweep_frees_abandoned_entries_and_keeps_live_ones`] pins what it
/// then does. The table is engine-local, and it cannot be watched from a
/// retained receiver either: an entry whose receiver is retained is exactly
/// the entry the sweep must keep. The timeout turns "never woke" into a
/// failure rather than a hang.
#[tokio::test]
async fn an_evicted_cancellation_is_still_reclaimed() {
    use core::future::Future;

    let (handle, cmd_rx, prune_rx) = test_handle_capped(1);
    let config = ClientConfig {
        reconnect: false,
        // Nothing but the signal may reclaim.
        keepalive_interval: None,
        ..Default::default()
    };
    let ctx = crate::RuntimeContext::new(Arc::new(crate::executor::test_support::NoopRuntimeOps));
    let engine = drive_connection(
        Box::new(SilentPeer::default()),
        &NullCodec,
        &cmd_rx,
        &prune_rx,
        &config,
        &ctx,
    );

    let exercise = async {
        // Queue the call, then let the engine file it *while the caller
        // still waits* — a receiver already dropped when the command is
        // read is discarded on arrival and never reaches the table.
        let mut call = Box::pin(handle.call("hang", Payload::from(&b"x"[..])));
        core::future::poll_fn(|cx| {
            assert!(
                call.as_mut().poll(cx).is_pending(),
                "the call must not resolve"
            );
            core::task::Poll::Ready(())
        })
        .await;
        tokio::task::yield_now().await;
        assert!(cmd_rx.is_empty(), "the engine must have filed the call");

        // Abandon it: the queue is drained, so the cancellation lands. No
        // yield before the write — the engine must not get the chance to
        // remove the entry by id.
        drop(call);
        assert_eq!(cmd_rx.len(), 1, "the cancellation must be queued");
        assert!(prune_rx.is_empty(), "nothing has fallen back to a sweep");
        handle
            .write("tele", Payload::from(&b"x"[..]))
            .expect("the engine is still running");
        assert_eq!(prune_rx.len(), 1, "the eviction must have been noticed");

        // The wake-up is the point: the engine is parked on a silent peer
        // with keepalive off, so only the signal itself can reach it.
        tokio::time::timeout(core::time::Duration::from_secs(5), async {
            while !prune_rx.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the signal must wake the engine and be consumed");
    };

    futures_util::pin_mut!(engine);
    futures_util::pin_mut!(exercise);
    match futures_util::future::select(engine, exercise).await {
        futures_util::future::Either::Left(_) => panic!("the engine ended early"),
        futures_util::future::Either::Right(((), _)) => {}
    }
}

/// The cap is a hard bound: nothing drains this channel, as during a pending
/// dial or a backoff sleep.
#[tokio::test]
async fn the_command_queue_is_bounded() {
    let (handle, cmd_rx, _prune_rx) = test_handle_capped(2);

    for topic in ["one", "two", "three", "four"] {
        handle
            .write(topic, Payload::from(&b"x"[..]))
            .expect("the engine is still running");
    }

    assert_eq!(cmd_rx.len(), 2, "the backlog must not exceed the cap");
    // Oldest-first: the two most recent writes are the ones kept.
    for expected in ["three", "four"] {
        match cmd_rx.try_recv() {
            Ok(ClientCmd::Write { topic, .. }) => assert_eq!(topic, expected),
            _ => panic!("the newest writes must survive eviction"),
        }
    }
}

/// `usize::MAX` was the old "unbounded" sentinel; preallocated, it would abort
/// in the allocator, so it has to degrade to the ceiling instead.
#[tokio::test]
async fn an_unbounded_cap_degrades_to_the_ceiling() {
    struct UndialableRemote;
    impl Dialer for UndialableRemote {
        fn connect(&self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
            Box::pin(async { Err(TransportError::Closed) })
        }
    }

    let config = ClientConfig {
        max_offline_queue: usize::MAX,
        ..Default::default()
    };
    let ctx = crate::RuntimeContext::new(Arc::new(crate::executor::test_support::NoopRuntimeOps));
    let (handle, _engine) = run_client(UndialableRemote, NullCodec, config, ctx);

    handle
        .write("t", Payload::from(&b"x"[..]))
        .expect("the engine is still running");
}

/// A zero cap is clamped, not honored: every command reaches the engine
/// through this channel, so a zero-capacity one could never deliver.
#[tokio::test]
async fn a_zero_cap_is_clamped_to_one() {
    let (handle, cmd_rx, _prune_rx) = test_handle_capped(0);
    let (reply, rx) = oneshot::channel();

    handle
        .enqueue(ClientCmd::Call {
            id: 1,
            method: String::from("one"),
            params: Payload::from(&b"x"[..]),
            reply,
        })
        .expect("the engine is still running");
    handle
        .write("later", Payload::from(&b"x"[..]))
        .expect("the engine is still running");

    assert_eq!(cmd_rx.len(), 1, "a clamped cap still holds exactly one");
    assert!(rx.await.is_err(), "and the evicted caller learns it");
}

/// The cap is the channel's own, so it holds across cloned handles without an
/// admission lock: a full channel evicts rather than admitting a second sender.
#[tokio::test]
async fn cloned_handles_cannot_exceed_the_cap() {
    let (handle, cmd_rx, _prune_rx) = test_handle_capped(1);
    let other = handle.clone();

    handle
        .write("first", Payload::from(&b"x"[..]))
        .expect("the engine is still running");
    other
        .write("second", Payload::from(&b"x"[..]))
        .expect("the engine is still running");

    assert_eq!(cmd_rx.len(), 1, "clones share one bounded channel");
    match cmd_rx.try_recv() {
        Ok(ClientCmd::Write { topic, .. }) => assert_eq!(topic, "second"),
        _ => panic!("oldest-first eviction must hold across clones"),
    }
}

/// A trimmed command is not silently lost: it carried the only sender of its
/// reply channel, so the caller resolves rather than awaiting a reply the
/// engine will never send.
#[tokio::test]
async fn a_trimmed_call_fails_its_caller() {
    let (handle, _cmd_rx, _prune_rx) = test_handle_capped(1);
    let (reply, rx) = oneshot::channel();
    handle
        .enqueue(ClientCmd::Call {
            id: 1,
            method: String::from("one"),
            params: Payload::from(&b"x"[..]),
            reply,
        })
        .expect("the engine is still running");

    // At cap 1 the next command displaces it.
    handle
        .write("later", Payload::from(&b"x"[..]))
        .expect("the engine is still running");

    assert!(
        rx.await.is_err(),
        "a trimmed call must not leave its caller waiting"
    );
}

type SubSinks = HashMap<String, Sender<Result<SubUpdate, RpcError>>>;
type SubCursors = HashMap<String, u64>;

/// One sink and its cursor, for driving [`deliver`] directly.
fn test_sink() -> (SubSinks, SubCursors, Receiver<Result<SubUpdate, RpcError>>) {
    let (tx, rx) = async_channel::bounded(SUBSCRIBE_CHANNEL_CAP);
    let mut subs = HashMap::new();
    subs.insert(String::from("1"), tx);
    (subs, HashMap::new(), rx)
}

fn deliver_event(subs: &mut SubSinks, last_seq: &mut SubCursors, seq: u64) {
    deliver(
        subs,
        last_seq,
        "1",
        seq,
        SubUpdate::new(Payload::from(&b"x"[..]), crate::Stamp::from_nanos(0)),
        Delivery::Event,
    );
}

/// `seq` arrives off the wire unvalidated, so the ceiling is reachable by a
/// peer. Computing the gap as `prev + 1` panicked in debug on the *next*
/// frame, and wrapped to a nonsense gap in release.
#[tokio::test]
async fn a_saturated_sequence_does_not_overflow() {
    let (mut subs, mut last_seq, rx) = test_sink();

    deliver_event(&mut subs, &mut last_seq, u64::MAX);
    deliver_event(&mut subs, &mut last_seq, 5);

    let skips: Vec<u64> = core::iter::from_fn(|| rx.try_recv().ok())
        .map(|u| u.unwrap().skipped)
        .collect();
    assert_eq!(skips.len(), 2, "both frames must be delivered");
    assert_eq!(skips[1], 0, "a saturated cursor cannot manufacture a gap");
}

/// A repeated or reordered `seq` is delivered but must not rewind the cursor:
/// a lower cursor makes the *next* valid frame look like a jump, reporting a
/// loss that never happened.
#[tokio::test]
async fn a_rewound_sequence_does_not_invent_a_gap() {
    let (mut subs, mut last_seq, rx) = test_sink();

    for seq in [1, 3, 2, 4] {
        deliver_event(&mut subs, &mut last_seq, seq);
    }

    let skips: Vec<u64> = core::iter::from_fn(|| rx.try_recv().ok())
        .map(|u| u.unwrap().skipped)
        .collect();
    assert_eq!(
        skips,
        alloc::vec![0, 1, 0, 0],
        "only the real gap (2 missing between 1 and 3) may be reported"
    );
    assert_eq!(last_seq.get("1"), Some(&4));
}

/// A slow subscriber whose sink is already full of updates must still see
/// the terminal error, not buffered values followed by a bare EOF.
#[tokio::test]
async fn a_terminal_error_survives_a_full_sink() {
    let (mut subs, mut last_seq, rx) = test_sink();

    for seq in 1..=(SUBSCRIBE_CHANNEL_CAP as u64 + 10) {
        deliver_event(&mut subs, &mut last_seq, seq);
    }
    end_sub(&mut subs, &mut last_seq, "1", RpcError::Protocol);

    let items: Vec<_> = core::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert_eq!(items.len(), SUBSCRIBE_CHANNEL_CAP);
    assert!(items[..items.len() - 1].iter().all(Result::is_ok));
    assert!(matches!(items.last(), Some(Err(RpcError::Protocol))));
    assert!(rx.is_closed(), "the sink is dropped after the error");
}

/// The mirror's loss contract (see [`inbound_pump`]): a server-reported gap
/// is stepped over, not treated as a fault. The update *carrying* `skipped`
/// is itself produced into the local record — it is current state, which is
/// all a mirror promises — and delivery continues past it.
#[tokio::test]
async fn a_mirror_gap_still_routes_and_keeps_mirroring() {
    let seen: Arc<spin::Mutex<Vec<Vec<u8>>>> = Arc::new(spin::Mutex::new(Vec::new()));
    let recorder = seen.clone();
    let ingest: crate::connector::IngestFn = Arc::new(move |_ctx, _m, bytes: &[u8]| {
        recorder.lock().push(bytes.to_vec());
        Ok(crate::IngestOutcome::current())
    });
    let route = crate::router::CompiledRoute::exact("tele", ingest);
    let router = crate::router::Router::new(&crate::ExactGrammar, alloc::vec![route]);
    let ctx = crate::RuntimeContext::new(Arc::new(crate::executor::test_support::NoopRuntimeOps));
    let inbound = crate::InboundDispatch::from_parts(router, ctx);
    let (handle, cmd_rx, _prune_rx) = test_handle();

    let pump = inbound_pump(handle, inbound, Arc::from("tele"));
    let remote = async {
        // The pump subscribes on its first poll; take its sink and play a
        // gapped update followed by a clean one.
        let events = match cmd_rx.recv().await {
            Ok(ClientCmd::Subscribe { events, .. }) => events,
            _ => panic!("the pump must subscribe"),
        };
        for (data, skipped) in [(&b"gapped"[..], 7u64), (&b"after"[..], 0)] {
            events
                .send(Ok(SubUpdate::new(
                    Payload::from(data),
                    crate::Stamp::from_nanos(0),
                )
                .with_skipped(skipped)))
                .await
                .expect("the pump holds the receiver");
        }
        // Dropping the sink ends the stream, so the pump returns.
        drop(events);
    };
    #[cfg(feature = "std")]
    let gap_seen = Arc::new(core::sync::atomic::AtomicUsize::new(0));
    #[cfg(feature = "std")]
    let pump = crate::session::events::capture::future(
        {
            let seen = gap_seen.clone();
            Arc::new(move |event| match event {
                SessionEvent::MirrorGap { topic, skipped } => {
                    assert_eq!((*topic, *skipped), ("tele", 7));
                    seen.fetch_add(1, Ordering::Relaxed);
                }
                other => panic!("unexpected event: {other:?}"),
            })
        },
        pump,
    );
    futures_util::future::join(pump, remote).await;
    #[cfg(feature = "std")]
    assert_eq!(gap_seen.load(Ordering::Relaxed), 1);

    let seen = seen.lock();
    assert_eq!(seen.len(), 2, "the gap must not drop or end the mirror");
    assert_eq!(
        seen[0].as_slice(),
        b"gapped",
        "the update reporting the gap must still be produced"
    );
    assert_eq!(seen[1].as_slice(), b"after", "mirroring must continue");
}
