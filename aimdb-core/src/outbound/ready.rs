//! Which outbound routes woke since they were last polled.
//!
//! Each route's reader is polled with a waker of its own. Waking it sets the
//! route's bit and wakes the transport task, which then polls only routes
//! whose bit is set. Wakers take no lock, so they may fire from any context,
//! including a producer that preempts the transport task.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::task::Wake;
use core::sync::atomic::Ordering;
use core::task::Waker;

use futures_util::task::AtomicWaker;
use portable_atomic::AtomicU32;

/// Dense route index, `0..len`.
pub(crate) type RouteId = usize;

/// State the route wakers share with the transport task.
struct Shared {
    /// One bit per route: woken since it was last taken.
    ready: Box<[AtomicU32]>,
    /// The transport task.
    task: AtomicWaker,
}

impl Shared {
    fn set(&self, id: RouteId) {
        if let Some(word) = self.ready.get(id / 32) {
            word.fetch_or(bit(id), Ordering::Release);
        }
    }

    fn clear(&self, id: RouteId) {
        if let Some(word) = self.ready.get(id / 32) {
            word.fetch_and(!bit(id), Ordering::Acquire);
        }
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
pub(crate) struct Pass {
    start: RouteId,
    /// Positions after `start` already scanned.
    offset: usize,
}

impl ReadyRoutes {
    /// `len` routes, all open and all ready: no reader has registered a waker
    /// yet.
    pub(crate) fn new(len: usize) -> Self {
        let words = len.div_ceil(32);
        let mut open = alloc::vec![0u32; words].into_boxed_slice();
        for id in 0..len {
            if let Some(word) = open.get_mut(id / 32) {
                *word |= bit(id);
            }
        }
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

    fn len(&self) -> usize {
        self.wakers.len()
    }

    /// Register the transport task. Call it before taking routes, so a route
    /// that wakes after the scan comes up empty still wakes the task.
    pub(crate) fn register(&self, waker: &Waker) {
        self.shared.task.register(waker);
    }

    /// The waker to poll route `id`'s reader with.
    pub(crate) fn waker(&self, id: RouteId) -> Option<&Waker> {
        self.wakers.get(id)
    }

    /// Start a pass at the route after the one served last.
    pub(crate) fn pass(&self) -> Pass {
        Pass {
            start: if self.len() == 0 {
                0
            } else {
                (self.cursor + 1) % self.len()
            },
            offset: 0,
        }
    }

    /// The next open route in `pass` that woke, with its bit cleared.
    ///
    /// Clearing before the caller polls means a wake that lands during the
    /// poll sets the bit again: at most a spurious re-poll, never a lost
    /// wake-up.
    pub(crate) fn take_next(&self, pass: &mut Pass) -> Option<RouteId> {
        let len = self.len();
        let pos = pass.start + pass.offset;
        let id = if pos < len {
            self.first_ready(pos, len)
                .or_else(|| self.first_ready(0, pass.start))
        } else {
            self.first_ready(pos - len, pass.start)
        }?;
        pass.offset = if id >= pass.start {
            id - pass.start
        } else {
            id + len - pass.start
        } + 1;
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
            let ready = self.shared.ready.get(w)?.load(Ordering::Acquire);
            let mut bits = ready & self.open.get(w).copied().unwrap_or(0);
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

    /// Route `id` yielded a value. Its buffer may hold more, and its waker
    /// does not fire again for values already in it, so its bit is set
    /// again; the cursor moves to it, so every other ready route is served
    /// first.
    pub(crate) fn served(&mut self, id: RouteId) {
        self.shared.set(id);
        self.cursor = id;
    }

    /// Route `id`'s buffer closed; it is never taken again.
    pub(crate) fn close(&mut self, id: RouteId) {
        if let Some(word) = self.open.get_mut(id / 32) {
            if *word & bit(id) != 0 {
                *word &= !bit(id);
                self.open_count -= 1;
            }
        }
    }

    /// Every route is closed (or there were none).
    pub(crate) fn is_done(&self) -> bool {
        self.open_count == 0
    }

    /// Some open route woke and has not been taken.
    pub(crate) fn any_ready(&self) -> bool {
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
    use core::sync::atomic::AtomicBool;

    /// Drives a [`ReadyRoutes`] the way `poll_stage` does, with a queue per
    /// route standing in for its reader.
    struct Sim {
        ready: ReadyRoutes,
        queues: Vec<VecDeque<u32>>,
    }

    impl Sim {
        fn new(len: usize) -> Self {
            Self {
                ready: ReadyRoutes::new(len),
                queues: (0..len).map(|_| VecDeque::new()).collect(),
            }
        }

        fn produce(&mut self, id: RouteId, v: u32) {
            self.queues[id].push_back(v);
            self.ready.waker(id).unwrap().wake_by_ref();
        }

        /// One call: the first route in the pass with a value, or `None`.
        fn next(&mut self) -> Option<RouteId> {
            let mut pass = self.ready.pass();
            while let Some(id) = self.ready.take_next(&mut pass) {
                if self.queues[id].pop_front().is_some() {
                    self.ready.served(id);
                    return Some(id);
                }
            }
            None
        }
    }

    fn drain(ready: &ReadyRoutes) -> Vec<RouteId> {
        let mut pass = ready.pass();
        core::iter::from_fn(|| ready.take_next(&mut pass)).collect()
    }

    #[derive(Default)]
    struct Flag(AtomicBool);

    impl Wake for Flag {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[test]
    fn serves_ready_routes_round_robin() {
        let mut sim = Sim::new(3);
        for v in 0..3 {
            for id in 0..3 {
                sim.produce(id, v);
            }
        }
        let order: Vec<_> = core::iter::from_fn(|| sim.next()).collect();
        assert_eq!(order, [0, 1, 2, 0, 1, 2, 0, 1, 2]);
    }

    #[test]
    fn a_hot_route_does_not_starve_a_quiet_one() {
        let mut sim = Sim::new(2);
        for v in 0..60 {
            sim.produce(0, v);
        }
        sim.produce(1, 0);
        assert_eq!((sim.next(), sim.next()), (Some(0), Some(1)));

        // Mid-stream: the quiet route is served right after the hot one.
        for _ in 0..5 {
            assert_eq!(sim.next(), Some(0));
        }
        sim.produce(1, 1);
        assert_eq!(sim.next(), Some(1));
    }

    #[test]
    fn a_route_woken_by_a_producer_wakes_the_task() {
        let ready = ReadyRoutes::new(3);
        assert_eq!(drain(&ready), [0, 1, 2]);
        let flag = Arc::new(Flag::default());
        ready.register(&Waker::from(flag.clone()));

        ready.waker(2).unwrap().wake_by_ref();
        assert!(flag.0.load(Ordering::Acquire));
        assert_eq!(drain(&ready), [2]);
    }

    #[test]
    fn a_wake_during_the_poll_is_not_lost() {
        let ready = ReadyRoutes::new(1);
        let mut pass = ready.pass();
        assert_eq!(ready.take_next(&mut pass), Some(0));
        // The reader registers its waker and a producer fires it before the
        // poll returns `Pending`.
        ready.waker(0).unwrap().wake_by_ref();
        assert!(ready.any_ready());
        assert_eq!(drain(&ready), [0]);
    }

    #[test]
    fn a_pass_takes_each_route_at_most_once() {
        let ready = ReadyRoutes::new(3);
        let mut pass = ready.pass();
        assert_eq!(ready.take_next(&mut pass), Some(0));
        ready.waker(0).unwrap().wake_by_ref();
        let rest: Vec<_> = core::iter::from_fn(|| ready.take_next(&mut pass)).collect();
        assert_eq!(rest, [1, 2]);
        assert!(ready.any_ready(), "route 0 waits for the next pass");
    }

    #[test]
    fn closed_routes_are_never_taken() {
        let mut ready = ReadyRoutes::new(3);
        ready.close(1);
        ready.close(1);
        ready.waker(1).unwrap().wake_by_ref();
        assert_eq!(drain(&ready), [0, 2]);
        assert!(!ready.is_done());
        ready.close(0);
        ready.close(2);
        assert!(ready.is_done());
        ready.waker(1).unwrap().wake_by_ref();
        assert!(!ready.any_ready());
    }

    #[test]
    fn no_routes_is_done_at_once() {
        let ready = ReadyRoutes::new(0);
        assert!(ready.is_done());
        assert!(drain(&ready).is_empty());
        assert!(ready.waker(0).is_none());
    }

    #[test]
    fn scans_across_word_boundaries() {
        for len in [1, 31, 32, 33, 64, 256] {
            let mut ready = ReadyRoutes::new(len);
            assert_eq!(drain(&ready), (0..len).collect::<Vec<_>>(), "len {len}");
            assert!(!ready.any_ready());

            let woken: Vec<_> = [0, 31, 32, len - 1]
                .into_iter()
                .filter(|&id| id < len)
                .collect();
            for &id in &woken {
                ready.waker(id).unwrap().wake_by_ref();
            }
            let mut expected = woken.clone();
            expected.dedup();
            assert_eq!(drain(&ready), expected, "len {len}");

            // From the middle: wraps past the end back to the cursor.
            if len > 33 {
                ready.served(31);
                ready.waker(0).unwrap().wake_by_ref();
                ready.waker(len - 1).unwrap().wake_by_ref();
                assert_eq!(drain(&ready), [len - 1, 0, 31], "len {len}");
            }
        }
    }

    /// Producers on other threads wake routes while the transport task
    /// takes them: nothing is lost and the task never stalls.
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
        let mut ready = ReadyRoutes::new(ROUTES);
        let pending: Arc<Vec<AtomicU64>> =
            Arc::new((0..ROUTES).map(|_| AtomicU64::new(0)).collect());
        let task = Waker::from(Arc::new(Unpark(thread::current())));

        let producers: Vec<_> = (0..4)
            .map(|t| {
                let pending = pending.clone();
                let wakers: Vec<Waker> = (0..ROUTES)
                    .map(|id| ready.waker(id).unwrap().clone())
                    .collect();
                thread::spawn(move || {
                    for i in 0..PER_PRODUCER as usize {
                        let id = (i * 7 + t * 13) % ROUTES;
                        pending[id].fetch_add(1, Ordering::Release);
                        wakers[id].wake_by_ref();
                    }
                })
            })
            .collect();

        let deadline = Instant::now() + Duration::from_secs(30);
        let mut total = 0;
        while total < 4 * PER_PRODUCER {
            ready.register(&task);
            let mut pass = ready.pass();
            let mut got = false;
            while let Some(id) = ready.take_next(&mut pass) {
                let n = pending[id].swap(0, Ordering::Acquire);
                if n > 0 {
                    total += n;
                    ready.served(id);
                    got = true;
                }
            }
            if !got {
                assert!(Instant::now() < deadline, "stalled at {total}");
                thread::park_timeout(Duration::from_millis(100));
            }
        }
        for p in producers {
            p.join().unwrap();
        }
        assert_eq!(total, 4 * PER_PRODUCER);
    }
}
