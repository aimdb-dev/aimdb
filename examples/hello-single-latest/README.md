# hello-single-latest: SingleLatest buffer demo

The `SingleLatest` buffer stores the current value of a record. Each new writes replace the previous value, so subscribers read the latest value instead of replaying every intermediate update. This is useful for feature flags, configuration, UI state, and other records where stale values can be skipped.

## How it works

This example registers a `FeatureFlag` record with:
- `BufferCfg::SingleLatest`
- an async `.tap()` that observes rollout updates

The program attaches AimDB through its sync API, and publishes rollout updates using the sync, blocking `producer.set()` method. The tap is managed by AimDB's internal async runtime, so the application can use a synchronous `main` function without `async` or `#[tokio::main]`.

The synchronous producer publishes one update per second. If multiple values are written before the tap reads them, SingleLatest retains only the newest value.

## How to run

From the workspace root, run:

```bash
cargo run -p hello-single-latest-async
```

Expected output includes lines similar to:

```text
=== AimDB SingleLatest Sync Demo ===

1. Building database and attaching for sync API...
2. Creating producer and producing values...
3. Producing rollout percentage values
   Main: Setting FeatureFlag 0%
Current rollout percentage: 0%
   Main: Setting FeatureFlag 25%
Current rollout percentage: 25%
   Main: Setting FeatureFlag 50%
Current rollout percentage: 50%
   Main: Setting FeatureFlag 75%
Current rollout percentage: 75%
   Main: Setting FeatureFlag 100%
Current rollout percentage: 100%
4. Shutting down...
Done. The sync producer published rollout updates observed by a SingleLatest tap.
```