//! Which outbound routes woke since they were last polled.
//!
//! Each route's reader is polled with a waker of its own. Waking it sets the
//! route's bit and wakes the transport task, which then polls only routes
//! whose bit is set. Wakers take no lock, so they may fire from any context,
//! including a producer that preempts the transport task.
//!
//! [`ReadyRoutes::poll_ready`] runs the whole loop, so the orderings that
//! keep a wake-up from being lost are kept here and nowhere else. Getting one
//! wrong stalls a route for good: a reader that returned a value keeps no
//! waker, so nothing would set its bit again.
//!
//! The transport task must not outrank its producers: run it at the same or
//! a lower priority than every task or interrupt that writes its routes'
//! records. `poll_ready` registers the task with an `AtomicWaker`, and a
//! registration that finds a wake still in progress wakes the task again
//! instead of waiting. A transport that preempted that wake is then polled
//! again and again, and on one core the producer never gets to finish it.
//!
//! A buffer may keep a route's waker after its reader is gone (embassy-sync's
//! `PubSubChannel` does). The producer's next write then drops the last
//! clone, which frees this set and wakes the finished task from the
//! producer's context.
//!
//! `futures_util`'s `SelectAll` does the same job but allocates per message.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::task::Wake;
use core::sync::atomic::Ordering;
use core::task::{Context, Poll, Waker};

use futures_util::task::AtomicWaker;
use portable_atomic::AtomicU32;

use super::RouteId;

/// Skips in a row one route may take before [`ReadyRoutes::poll_ready`]
/// moves on.
const SKIP_BUDGET: usize = 32;

/// What polling one route's reader did, as reported to
/// [`ReadyRoutes::poll_ready`].
pub(crate) enum Polled {
    /// Nothing to send. The reader kept the route's waker, or woke it itself
    /// (Tokio's readers do once the task's budget is spent).
    Pending,
    /// A value was staged; `poll_ready` returns this route.
    Staged,
    /// A value was taken but not staged (topic overflow, serializer error),
    /// or the reader lagged. The reader kept no waker, so the route is
    /// polled again at once, up to [`SKIP_BUDGET`] times in a row.
    Skipped,
    /// The reader's buffer is closed; the route is never polled again.
    Closed,
}

/// State the route wakers share with the transport task.
struct Shared {
    /// One bit per route: woken since it was last taken.
    ready: Box<[AtomicU32]>,
    /// The transport task.
    task: AtomicWaker,
}

// One read-modify-write each: 32 routes share a word, and a load followed by
// a store would erase a bit another route set in between.
impl Shared {
    fn set(&self, id: RouteId) {
        self.ready[id / 32].fetch_or(bit(id), Ordering::Release);
    }

    fn clear(&self, id: RouteId) {
        self.ready[id / 32].fetch_and(!bit(id), Ordering::Acquire);
    }
}

struct RouteWake {
    id: RouteId,
    shared: Arc<Shared>,
}

impl Wake for RouteWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref()
    }

    // Runs in the producer's context, possibly an interrupt-priority task
    // that preempted the transport task: it must never take a lock.
    fn wake_by_ref(self: &Arc<Self>) {
        self.shared.set(self.id);
        self.shared.task.wake();
    }
}

fn bit(id: RouteId) -> u32 {
    1 << (id % 32)
}

/// The ready set of one connector's outbound routes, owned by its transport
/// task.
pub(crate) struct ReadyRoutes {
    shared: Arc<Shared>,
    /// Built once; polling route `id` uses `wakers[id]`.
    wakers: Box<[Waker]>,
    /// One bit per route that is not closed.
    open: Box<[u32]>,
    open_count: usize,
    /// The route served last; the next pass starts after it.
    cursor: RouteId,
}

/// One round-robin pass: every route at most once, starting after the
/// cursor.
///
/// The bound is what ends a call. A reader may wake its own waker before it
/// returns `Pending` (spurious wakes are allowed), setting its bit again
/// while it is polled; a scan that went back for set bits could then never
/// stop. Tokio's readers, once the task's budget is spent, defer that wake
/// until the task yields.
struct Pass {
    start: RouteId,
    /// The next position to scan, counted from `start` without wrapping:
    /// `start..len` are routes `start..len`, `len..start + len` are routes
    /// `0..start`.
    next: usize,
}

impl ReadyRoutes {
    /// `len` routes, all open and all ready: no reader has registered a waker
    /// yet.
    pub(crate) fn new(len: usize) -> Self {
        let open: Box<[u32]> = (0..len.div_ceil(32))
            .map(|w| {
                let rest = len - w * 32;
                if rest < 32 {
                    (1 << rest) - 1
                } else {
                    u32::MAX
                }
            })
            .collect();
        let shared = Arc::new(Shared {
            ready: open.iter().map(|&w| AtomicU32::new(w)).collect(),
            task: AtomicWaker::new(),
        });
        let wakers = (0..len)
            .map(|id| {
                Waker::from(Arc::new(RouteWake {
                    id,
                    shared: shared.clone(),
                }))
            })
            .collect();
        Self {
            shared,
            wakers,
            open,
            open_count: len,
            cursor: len.saturating_sub(1),
        }
    }

    /// Poll the routes that woke, in round-robin order, until one stages a
    /// value.
    ///
    /// `poll_route(id, route_cx)` polls route `id`'s reader with `route_cx`,
    /// which carries that route's own waker, and reports what happened. It
    /// must not poll the reader with any other context: a reader holding the
    /// task's waker wakes the task without marking its route.
    ///
    /// - `Ready(Some(id))`: route `id` staged a value. Every other route that
    ///   is ready is served before `id` is served again. A route that keeps
    ///   skipping is polled at most [`SKIP_BUDGET`] times in a row, then
    ///   again in a later pass.
    /// - `Ready(None)`: every route is closed, or there were none. Final.
    /// - `Pending`: no woken route had a value. The task is woken when one
    ///   does.
    pub(crate) fn poll_ready(
        &mut self,
        cx: &mut Context<'_>,
        mut poll_route: impl FnMut(RouteId, &mut Context<'_>) -> Polled,
    ) -> Poll<Option<RouteId>> {
        if self.open_count == 0 {
            return Poll::Ready(None);
        }
        // Before any bit is read. Every wake takes the stored waker out, so
        // a route that wakes after the scan has passed its word finds this
        // one instead of an empty slot.
        self.shared.task.register(cx.waker());
        let mut pass = self.pass();
        while let Some(id) = self.take_next(&mut pass) {
            let mut route_cx = Context::from_waker(&self.wakers[id]);
            let mut skips = 0;
            loop {
                match poll_route(id, &mut route_cx) {
                    Polled::Pending => break,
                    Polled::Staged => {
                        // `served` sets the bit without waking the task, so
                        // nothing may come between it and `Ready`.
                        self.served(id);
                        return Poll::Ready(Some(id));
                    }
                    // Moving on would leave the route with a clear bit and a
                    // reader that kept no waker, so it is polled again. After
                    // `SKIP_BUDGET` skips in a row its own waker sets its bit
                    // and wakes the task instead: a producer that keeps up
                    // with the skips cannot hold the call.
                    Polled::Skipped => {
                        skips += 1;
                        if skips == SKIP_BUDGET {
                            route_cx.waker().wake_by_ref();
                            break;
                        }
                    }
                    Polled::Closed => {
                        self.close(id);
                        break;
                    }
                }
            }
        }
        if self.open_count == 0 {
            Poll::Ready(None)
        } else {
            Poll::Pending
        }
    }

    fn len(&self) -> usize {
        self.wakers.len()
    }

    /// Start a pass at the route after the one served last.
    fn pass(&self) -> Pass {
        let start = match self.len() {
            0 => 0,
            len => (self.cursor + 1) % len,
        };
        Pass { start, next: start }
    }

    /// The next open route in `pass` that woke, with its bit cleared.
    ///
    /// Clearing before the route is polled means a wake that lands during
    /// the poll sets the bit again: at most a spurious re-poll, never a lost
    /// wake-up.
    fn take_next(&self, pass: &mut Pass) -> Option<RouteId> {
        let len = self.len();
        let id = if pass.next < len {
            self.first_ready(pass.next, len)
                .or_else(|| self.first_ready(0, pass.start))
        } else {
            self.first_ready(pass.next - len, pass.start)
        }?;
        pass.next = if id >= pass.start { id } else { id + len } + 1;
        self.shared.clear(id);
        Some(id)
    }

    /// First open, ready route in `lo..hi`.
    fn first_ready(&self, lo: RouteId, hi: RouteId) -> Option<RouteId> {
        if lo >= hi {
            return None;
        }
        let (first, last) = (lo / 32, (hi - 1) / 32);
        for w in first..=last {
            let mut bits = self.shared.ready[w].load(Ordering::Acquire) & self.open[w];
            if w == first {
                bits &= u32::MAX << (lo % 32);
            }
            let top = hi - w * 32;
            if w == last && top < 32 {
                bits &= (1 << top) - 1;
            }
            if bits != 0 {
                return Some(w * 32 + bits.trailing_zeros() as usize);
            }
        }
        None
    }

    /// Route `id` staged a value. Its buffer may hold more, and its waker
    /// does not fire again for values already in it, so its bit is set
    /// again; the cursor moves to it, so every other ready route is served
    /// first.
    fn served(&mut self, id: RouteId) {
        self.shared.set(id);
        self.cursor = id;
    }

    /// Route `id`'s buffer closed; it is never taken again.
    fn close(&mut self, id: RouteId) {
        let word = &mut self.open[id / 32];
        if *word & bit(id) != 0 {
            *word &= !bit(id);
            self.open_count -= 1;
        }
    }

    /// Some open route woke and has not been taken.
    #[cfg(test)]
    fn any_ready(&self) -> bool {
        self.shared
            .ready
            .iter()
            .zip(self.open.iter())
            .any(|(r, &o)| r.load(Ordering::Acquire) & o != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::VecDeque;
    use alloc::vec::Vec;
    use core::sync::atomic::AtomicUsize;

    /// Counts the transport task's wake-ups.
    #[derive(Default)]
    struct Count(AtomicUsize);

    impl Wake for Count {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref()
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    enum Value {
        Good(u32),
        /// Fails to serialize.
        Bad,
    }

    /// A route's reader, modelled on Tokio's: it keeps the waker only when
    /// it returns `Pending`, and a write wakes that waker and drops it.
    #[derive(Default)]
    struct Route {
        queue: VecDeque<Value>,
        waker: Option<Waker>,
        closed: bool,
        /// Good values accepted, in order.
        written: Vec<u32>,
        polls: usize,
    }

    impl Route {
        fn write(&mut self, value: Value) {
            if self.closed {
                return;
            }
            if let Value::Good(v) = value {
                self.written.push(v);
            }
            self.queue.push_back(value);
            if let Some(w) = self.waker.take() {
                w.wake();
            }
        }

        fn close(&mut self) {
            self.closed = true;
            if let Some(w) = self.waker.take() {
                w.wake();
            }
        }
    }

    /// Drives `poll_ready` the way `OutboundRoutes::poll_stage` does.
    struct Sim {
        ready: ReadyRoutes,
        routes: Vec<Route>,
        task: Arc<Count>,
        /// Values staged, in order.
        sent: Vec<(RouteId, u32)>,
        /// The next call's readers return `Pending` and wake themselves.
        budget_spent: bool,
        /// When route `.0` returns `Pending`, write `.2` into route `.1`
        /// before the poll returns: a producer racing the transport task.
        inject: Option<(RouteId, RouteId, u32)>,
    }

    impl Sim {
        fn new(len: usize) -> Self {
            Self {
                ready: ReadyRoutes::new(len),
                routes: (0..len).map(|_| Route::default()).collect(),
                task: Arc::default(),
                sent: Vec::new(),
                budget_spent: false,
                inject: None,
            }
        }

        fn write(&mut self, id: RouteId, v: u32) {
            self.routes[id].write(Value::Good(v));
        }

        fn wakes(&self) -> usize {
            self.task.0.load(Ordering::Relaxed)
        }

        fn take_wakes(&self) -> usize {
            self.task.0.swap(0, Ordering::Relaxed)
        }

        fn poll(&mut self) -> Poll<Option<RouteId>> {
            let waker = Waker::from(self.task.clone());
            let mut cx = Context::from_waker(&waker);
            let budget_spent = core::mem::take(&mut self.budget_spent);
            // Each route once per pass, plus one poll per value taken, plus
            // the value a racing producer may add: fail instead of spinning.
            let limit = self.routes.len() + self.queued() + 2;
            let mut polls = 0;
            let (routes, sent, inject) = (&mut self.routes, &mut self.sent, &mut self.inject);
            self.ready.poll_ready(&mut cx, |id, route_cx| {
                polls += 1;
                assert!(polls <= limit, "one call polled {polls} times");
                let route = &mut routes[id];
                route.polls += 1;
                if budget_spent {
                    route_cx.waker().wake_by_ref();
                    return Polled::Pending;
                }
                match route.queue.pop_front() {
                    Some(Value::Good(v)) => {
                        sent.push((id, v));
                        Polled::Staged
                    }
                    Some(Value::Bad) => Polled::Skipped,
                    None if route.closed => Polled::Closed,
                    None => {
                        route.waker = Some(route_cx.waker().clone());
                        if let Some((at, to, v)) = *inject {
                            if at == id {
                                *inject = None;
                                routes[to].write(Value::Good(v));
                            }
                        }
                        Polled::Pending
                    }
                }
            })
        }

        /// The next route served, or `None` once a call does not serve one.
        fn next(&mut self) -> Option<RouteId> {
            match self.poll() {
                Poll::Ready(id) => id,
                Poll::Pending => None,
            }
        }

        fn drain(&mut self) -> Vec<RouteId> {
            core::iter::from_fn(|| self.next()).collect()
        }

        fn polls(&self) -> Vec<usize> {
            self.routes.iter().map(|r| r.polls).collect()
        }

        fn queued(&self) -> usize {
            self.routes.iter().map(|r| r.queue.len()).sum()
        }
    }

    /// Every route in one pass, without polling any reader.
    fn take_all(ready: &ReadyRoutes) -> Vec<RouteId> {
        let mut pass = ready.pass();
        core::iter::from_fn(|| ready.take_next(&mut pass)).collect()
    }

    #[test]
    fn serves_ready_routes_round_robin() {
        let mut sim = Sim::new(3);
        for v in 0..3 {
            for id in 0..3 {
                sim.write(id, v);
            }
        }
        assert_eq!(sim.drain(), [0, 1, 2, 0, 1, 2, 0, 1, 2]);
    }

    #[test]
    fn a_hot_route_does_not_starve_a_quiet_one() {
        let mut sim = Sim::new(2);
        for v in 0..60 {
            sim.write(0, v);
        }
        sim.write(1, 0);
        assert_eq!((sim.next(), sim.next()), (Some(0), Some(1)));

        // Mid-stream: the quiet route is served right after the hot one.
        for _ in 0..5 {
            assert_eq!(sim.next(), Some(0));
        }
        sim.write(1, 1);
        assert_eq!(sim.next(), Some(1));
    }

    #[test]
    fn a_write_wakes_the_task_and_is_served() {
        let mut sim = Sim::new(3);
        assert!(sim.drain().is_empty());
        let wakes = sim.wakes();

        sim.write(2, 7);
        assert_eq!(sim.wakes(), wakes + 1);
        assert_eq!(sim.drain(), [2]);
        assert_eq!(sim.sent, [(2, 7)]);
    }

    #[test]
    fn a_wake_during_the_poll_is_not_lost() {
        let mut sim = Sim::new(2);
        assert!(sim.drain().is_empty());

        // A spurious wake gets route 1 polled. Its reader stores its waker
        // and a producer fires it before the poll returns `Pending`.
        sim.ready.wakers[1].wake_by_ref();
        sim.take_wakes();
        sim.inject = Some((1, 1, 5));
        assert!(sim.poll().is_pending());
        assert!(sim.take_wakes() > 0);
        assert_eq!(sim.drain(), [1]);
        assert_eq!(sim.sent, [(1, 5)]);
    }

    #[test]
    fn a_skipped_value_is_polled_past() {
        let mut sim = Sim::new(2);
        assert!(sim.drain().is_empty());

        sim.routes[1].write(Value::Bad);
        sim.write(1, 3);
        assert_eq!(sim.drain(), [1]);
        assert_eq!(sim.polls()[1], 4, "registered, skipped, staged, empty");

        // A skip with nothing behind it leaves the reader holding its waker.
        sim.routes[1].write(Value::Bad);
        assert!(sim.drain().is_empty());
        sim.write(1, 4);
        assert_eq!(sim.drain(), [1]);
        assert_eq!(sim.sent, [(1, 3), (1, 4)]);
    }

    #[test]
    fn a_route_that_keeps_skipping_does_not_hold_the_call() {
        // Route 0's values all fail to stage, and a producer that preempts
        // the transport refills it as fast as they are skipped. Route 1 has
        // a value waiting.
        let mut ready = ReadyRoutes::new(2);
        let task = Arc::new(Count::default());
        let waker = Waker::from(task.clone());
        let mut cx = Context::from_waker(&waker);
        let mut skips = 0;
        let polled = ready.poll_ready(&mut cx, |id, _| match id {
            0 => {
                skips += 1;
                assert!(skips <= SKIP_BUDGET, "one call skipped {skips} times");
                Polled::Skipped
            }
            _ => Polled::Staged,
        });
        assert_eq!(polled, Poll::Ready(Some(1)));
        assert_eq!(skips, SKIP_BUDGET);

        // Route 0 is marked and the task woken, so the next call polls it.
        assert!(task.0.load(Ordering::Relaxed) > 0);
        let mut polled_again = false;
        let _ = ready.poll_ready(&mut cx, |id, _| {
            polled_again |= id == 0;
            Polled::Pending
        });
        assert!(polled_again);
    }

    #[test]
    fn a_pass_starting_at_bit_31_takes_that_route_once() {
        // The wrap-around part of a pass stops below `start`. When `start` is
        // the top bit of a word, only that word's mask keeps the scan from
        // taking `start` again after its reader woke itself.
        let mut sim = Sim::new(64);
        assert!(sim.drain().is_empty());
        sim.ready.cursor = 30;
        sim.write(31, 1);
        let before = sim.polls()[31];

        sim.budget_spent = true;
        assert!(sim.poll().is_pending());
        assert_eq!(sim.polls()[31] - before, 1);
        assert_eq!(sim.drain(), [31]);
    }

    #[test]
    fn a_spent_budget_ends_the_call() {
        let mut sim = Sim::new(3);
        assert!(sim.drain().is_empty());
        for id in 0..3 {
            sim.write(id, id as u32);
        }
        let before = sim.polls();
        sim.take_wakes();

        // Every reader wakes itself while it is polled: one poll each, then
        // `Pending`, with the task woken to try again.
        sim.budget_spent = true;
        assert!(sim.poll().is_pending());
        let after: Vec<_> = sim
            .polls()
            .iter()
            .zip(&before)
            .map(|(a, b)| a - b)
            .collect();
        assert_eq!(after, [1, 1, 1]);
        assert!(sim.take_wakes() > 0);
        assert_eq!(sim.drain(), [0, 1, 2]);
    }

    #[test]
    fn closed_routes_are_never_polled_again() {
        let mut sim = Sim::new(3);
        sim.write(1, 9);
        sim.routes[1].close();
        assert_eq!(sim.drain(), [1]);
        assert_eq!(sim.sent, [(1, 9)], "values before the close still go out");
        let polls = sim.polls()[1];

        sim.ready.wakers[1].wake_by_ref();
        assert!(sim.drain().is_empty());
        assert_eq!(sim.polls()[1], polls);

        sim.routes[0].close();
        sim.routes[2].close();
        assert_eq!(sim.poll(), Poll::Ready(None));
        assert_eq!(sim.poll(), Poll::Ready(None), "final");
    }

    #[test]
    fn no_routes_is_done_at_once() {
        let mut sim = Sim::new(0);
        assert_eq!(sim.poll(), Poll::Ready(None));
        assert!(take_all(&sim.ready).is_empty());
    }

    #[test]
    fn scans_across_word_boundaries() {
        for len in [1, 31, 32, 33, 64, 256] {
            let mut ready = ReadyRoutes::new(len);
            assert_eq!(take_all(&ready), (0..len).collect::<Vec<_>>(), "len {len}");
            assert!(!ready.any_ready());

            let mut woken: Vec<_> = [0, 31, 32, len - 1]
                .into_iter()
                .filter(|&id| id < len)
                .collect();
            woken.dedup();
            for &id in &woken {
                ready.wakers[id].wake_by_ref();
            }
            assert_eq!(take_all(&ready), woken, "len {len}");

            // From the middle: wraps past the end back to the cursor.
            if len > 33 {
                ready.served(31);
                ready.wakers[0].wake_by_ref();
                ready.wakers[len - 1].wake_by_ref();
                assert_eq!(take_all(&ready), [len - 1, 0, 31], "len {len}");
            }
        }
    }

    /// xorshift64, so the randomized test needs no dependency and replays.
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n as u64) as usize
        }
    }

    /// Random writes, skipped values, closes, spent budgets and producers
    /// racing the poll, with the task polled only when it was woken or its
    /// last call served a value. Once it stops, every value written has gone
    /// out, in order per route: a lost wake-up leaves one behind.
    #[test]
    fn every_value_written_is_sent() {
        for seed in 1..=300u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let len = [1, 2, 5, 33, 64][rng.below(5)];
            let mut sim = Sim::new(len);
            let mut runnable = true;
            let mut v = 0;
            for _ in 0..400 {
                v += 1;
                match rng.below(12) {
                    0..=3 => sim.write(rng.below(len), v),
                    4 => sim.routes[rng.below(len)].write(Value::Bad),
                    5 if rng.below(10) == 0 => sim.routes[rng.below(len)].close(),
                    6 => sim.budget_spent = true,
                    7 => sim.inject = Some((rng.below(len), rng.below(len), v)),
                    _ => {
                        if runnable || sim.take_wakes() > 0 {
                            runnable = matches!(sim.poll(), Poll::Ready(Some(_)));
                        }
                    }
                }
            }
            let mut calls = 0;
            while runnable || sim.take_wakes() > 0 {
                runnable = matches!(sim.poll(), Poll::Ready(Some(_)));
                calls += 1;
                assert!(calls < 10_000, "seed {seed}: never parks");
            }
            for (id, route) in sim.routes.iter().enumerate() {
                let sent: Vec<u32> = sim
                    .sent
                    .iter()
                    .filter(|&&(r, _)| r == id)
                    .map(|&(_, v)| v)
                    .collect();
                assert_eq!(sent, route.written, "seed {seed}, route {id} of {len}");
            }
        }
    }

    /// Clearing one route's bit never erases a neighbour's bit set at the
    /// same time: 32 routes share a word.
    #[cfg(feature = "std")]
    #[test]
    fn clearing_a_bit_keeps_its_neighbours() {
        use std::sync::atomic::AtomicBool;
        use std::thread;

        let shared = ReadyRoutes::new(2).shared;
        let stop = Arc::new(AtomicBool::new(false));
        let clearer = {
            let (shared, stop) = (shared.clone(), stop.clone());
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    shared.clear(0);
                }
            })
        };
        // Only this thread touches bit 1, so it must read back as set.
        let mut erased = 0;
        for _ in 0..200_000 {
            shared.set(1);
            for _ in 0..16 {
                core::hint::spin_loop();
            }
            if shared.ready[0].load(Ordering::Acquire) & bit(1) == 0 {
                erased += 1;
            }
            shared.ready[0].fetch_and(!bit(1), Ordering::Relaxed);
        }
        stop.store(true, Ordering::Relaxed);
        clearer.join().unwrap();
        assert_eq!(erased, 0);
    }

    /// Producers on other threads wake routes while the transport task
    /// polls them: nothing is lost, and the task never sleeps through a
    /// wake-up.
    #[cfg(feature = "std")]
    #[test]
    fn concurrent_wakes_lose_nothing() {
        use portable_atomic::AtomicU64;
        use std::thread;
        use std::time::{Duration, Instant};

        struct Unpark(thread::Thread);
        impl Wake for Unpark {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }
        }

        const ROUTES: usize = 64;
        const PER_PRODUCER: u64 = 50_000;
        const TOTAL: u64 = 4 * PER_PRODUCER;
        const PARK: Duration = Duration::from_secs(5);
        let mut ready = ReadyRoutes::new(ROUTES);
        let pending: Arc<Vec<AtomicU64>> =
            Arc::new((0..ROUTES).map(|_| AtomicU64::new(0)).collect());
        let task = Waker::from(Arc::new(Unpark(thread::current())));
        let mut cx = Context::from_waker(&task);

        let producers: Vec<_> = (0..4)
            .map(|t| {
                let pending = pending.clone();
                let wakers = ready.wakers.to_vec();
                thread::spawn(move || {
                    for i in 0..PER_PRODUCER as usize {
                        let id = (i * 7 + t * 13) % ROUTES;
                        pending[id].fetch_add(1, Ordering::Release);
                        wakers[id].wake_by_ref();
                    }
                })
            })
            .collect();

        let mut total = 0;
        while total < TOTAL {
            let polled = ready.poll_ready(&mut cx, |id, _| {
                let n = pending[id].swap(0, Ordering::Acquire);
                total += n;
                if n > 0 {
                    Polled::Staged
                } else {
                    Polled::Pending
                }
            });
            if polled.is_pending() && total < TOTAL {
                // Every write after the scan unparks this thread, so a park
                // that runs out with work outstanding slept through a wake.
                let parked = Instant::now();
                thread::park_timeout(PARK);
                let stranded = pending.iter().any(|p| p.load(Ordering::Acquire) > 0);
                assert!(
                    parked.elapsed() < PARK || !stranded,
                    "slept through a wake-up at {total}"
                );
            }
        }
        for p in producers {
            p.join().unwrap();
        }
        assert_eq!(total, TOTAL);
    }
}
