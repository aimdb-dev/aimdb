# hello-single-latest-async: SingleLatest buffer demo

The `SingleLatest` buffer stores the current value for a record. New writes replace the previous value, so subscribers read the latest state instead of replaying every intermediate update. Use it for feature flags, configuration, UI state, or other records where stale values should be skipped.

## How it works

This example registers a `FeatureGate` record with:

- `BufferCfg::SingleLatest`
- an async `.source()` that publishes rollout percentages
- an async `.tap()` that observes the latest value whenever it changes

The source sends an initial burst of updates without waiting between writes. The tap prints the latest observed rollout, demonstrating that the buffer carries current state rather than a full event log.

## How to run

From the workspace root, run:

```bash
cargo run -p hello-single-latest-async
```

Expected output includes lines similar to:

```text
=== hello-single-latest-async: SingleLatest buffer demo ===

source published rollout: 0%
source published rollout: 10%
source published rollout: 25%
tap observed current rollout: 25%
source published rollout: 50%
tap observed current rollout: 50%
source published rollout: 100%
tap observed current rollout: 100%
Done. SingleLatest keeps only the current value for each subscriber.
```
