//! `AimDbBuilder::start_order_seed`: the seed reaches `build()`, and the
//! order it produces is the one the database reports and starts in.
#![cfg(feature = "start-order-seed")]

use std::sync::Arc;

use aimdb_core::executor::test_support::NoopRuntimeOps;
use aimdb_core::AimDbBuilder;

const KEYS: [&str; 8] = ["a", "b", "c", "d", "e", "f", "g", "h"];

async fn topo_order(seed: Option<u64>) -> Vec<String> {
    let mut builder = AimDbBuilder::new().runtime(Arc::new(NoopRuntimeOps));
    if let Some(seed) = seed {
        builder = builder.start_order_seed(seed);
    }
    for key in KEYS {
        builder.configure::<u32>(key, |_reg| {});
    }
    let (db, _runner) = builder.build().await.expect("build");
    db.inner().dependency_graph().topo_order().to_vec()
}

#[tokio::test]
async fn without_a_seed_records_start_in_registration_order() {
    assert_eq!(topo_order(None).await, KEYS);
}

#[tokio::test]
async fn a_seed_permutes_the_start_order_reproducibly() {
    let mut distinct = std::collections::HashSet::new();
    for seed in 0..16 {
        let order = topo_order(Some(seed)).await;
        assert_eq!(
            order,
            topo_order(Some(seed)).await,
            "seed {seed} must replay"
        );
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(sorted, KEYS, "seed {seed} must keep every record once");
        distinct.insert(order);
    }
    assert!(
        distinct.len() > 8,
        "only {} distinct orders",
        distinct.len()
    );
}
