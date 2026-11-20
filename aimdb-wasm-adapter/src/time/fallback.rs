pub(super) struct WasmClock;

impl WasmClock {
    pub(super) fn new() -> Self {
        Self
    }
}

impl aimdb_core::InstanceClock for WasmClock {
    fn now(&self) -> aimdb_core::Stamp {
        aimdb_core::Stamp::from_nanos(0)
    }
    fn unix_time(&self) -> Option<(u64, u32)> {
        None
    }
}

pub(super) fn sleep(_: core::time::Duration) -> aimdb_core::BoxFuture {
    alloc::boxed::Box::pin(core::future::ready(()))
}

pub(super) fn log(_: aimdb_core::LogLevel, _: &str) {}
