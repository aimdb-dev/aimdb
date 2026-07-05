//! B0 — Allocation counting for `migration_chain!`'s tree-free version
//! probe.
//!
//! Measures per-call allocation cost of `migrate_from_bytes` upgrading a
//! v1 payload to the current schema version. The two-pass probe scan
//! (peak allocation O(concrete struct)) replaces a full `serde_json::Value`
//! tree parse (peak allocation O(payload tree)) — a regression here means
//! the tree-free path stopped being tree-free.
//!
//! **Measurement model:** warm up `WARMUP_ITERS` calls, `reset()` the
//! counters, run `BATCH_SIZE` more calls, then `snapshot()` and divide by
//! `BATCH_SIZE` (same shape as [`aimdb_bench::harness::measure_b0`], which
//! doesn't apply directly here since there's no `Buffer`/`Reader` in play).
//!
//! Run `cargo bench -p aimdb-bench --bench b0_alloc_migration`; results
//! are written to `aimdb-bench/target/bench-results/b0_alloc_migration.json`.

use aimdb_bench::{
    payloads::{BATCH_SIZE, WARMUP_ITERS},
    reports::{write_reports, AllocReport},
};
use aimdb_data_contracts::{
    migration_chain, MigrationChain, MigrationError, MigrationStep, SchemaType,
};
use serde::{Deserialize, Serialize};

#[global_allocator]
static GLOBAL: aimdb_bench::alloc::CountingAllocator<std::alloc::System> =
    aimdb_bench::alloc::CountingAllocator(std::alloc::System);

// A `label: String` field keeps this fixture representative of real
// contracts (e.g. weather-mesh-common's `TemperatureV1.unit`) — an
// all-primitive struct would parse allocation-free regardless of the
// version-scan strategy and wouldn't show anything.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct SensorV1 {
    #[serde(default = "sensor_v1_version")]
    schema_version: u32,
    label: String,
    value: f32,
}
fn sensor_v1_version() -> u32 {
    1
}
impl SchemaType for SensorV1 {
    const NAME: &'static str = "bench_sensor";
    const VERSION: u32 = 1;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SensorV2 {
    #[serde(default = "sensor_v2_version")]
    schema_version: u32,
    label: String,
    value: f32,
}
fn sensor_v2_version() -> u32 {
    2
}
impl SchemaType for SensorV2 {
    const NAME: &'static str = "bench_sensor";
    const VERSION: u32 = 2;
}

struct SensorV1ToV2;
impl MigrationStep for SensorV1ToV2 {
    type Older = SensorV1;
    type Newer = SensorV2;
    const FROM_VERSION: u32 = 1;
    const TO_VERSION: u32 = 2;

    fn up(v1: SensorV1) -> Result<SensorV2, MigrationError> {
        Ok(SensorV2 {
            schema_version: 2,
            label: v1.label,
            value: v1.value,
        })
    }
    fn down(v2: SensorV2) -> Result<SensorV1, MigrationError> {
        Ok(SensorV1 {
            schema_version: 1,
            label: v2.label,
            value: v2.value,
        })
    }
}

migration_chain! {
    type Current = SensorV2;
    version_field = "schema_version";
    steps {
        SensorV1ToV2: SensorV1 => SensorV2,
    }
}

fn main() {
    println!("=== B0 Allocation Benchmark (migrate_from_bytes, tree-free version probe) ===");
    println!("  Warmup iters : {WARMUP_ITERS}");
    println!("  Batch size   : {BATCH_SIZE}");
    println!();

    let payload = serde_json::to_vec(&SensorV1 {
        schema_version: 1,
        label: "bench-sensor-01".to_string(),
        value: 42.5,
    })
    .expect("failed to serialize v1 fixture payload");

    for _ in 0..WARMUP_ITERS {
        let _ = SensorV2::migrate_from_bytes(&payload).expect("migration should succeed");
    }

    aimdb_bench::alloc::reset();
    for _ in 0..BATCH_SIZE {
        let _ = SensorV2::migrate_from_bytes(&payload).expect("migration should succeed");
    }
    let (allocs, bytes) = aimdb_bench::alloc::snapshot();
    let report = AllocReport::new(
        "MigrateFromBytes",
        "SensorV1ToV2",
        BATCH_SIZE,
        allocs,
        bytes,
    );
    report.print();

    write_reports("b0_alloc_migration", &[report]);
}
