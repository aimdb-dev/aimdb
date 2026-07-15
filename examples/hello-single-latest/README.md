# hello-single-latest: SingleLatest buffer demo

The `SingleLatest` buffer stores the current value of a record. Each new write replaces the previous value, so subscribers read the latest value instead of replaying every intermediate update. This is useful for feature flags, configuration, UI state, and other records where stale values can be skipped.

## How it works

This example registers a `FeatureFlag` record with:
- `BufferCfg::SingleLatest`
- 2 async `.tap()` that observe rollout updates
- One of the tap observers is "fast", that doesn't wait between receives
- One of the tap observers is "slow", that waits for 2000 ms between receives

The program attaches AimDB through its sync API, and publishes rollout updates using the sync, blocking `producer.set()` method. The taps are managed by AimDB's internal async runtime, so the application can use a synchronous `main` function without `async` or `#[tokio::main]`.

The synchronous producer publishes one update per second. If multiple values are written before the tap reads them, SingleLatest retains only the newest value. In the example, while the fast tap reads the value in the single latest slot, the slow one skips overwritten intermediate values. 

## How to run

From the workspace root, run:

```bash
cargo run -p hello-single-latest
```

Expected output includes lines similar to:

```text
=== AimDB SingleLatest Sync Demo ===

1. Building database and attaching for sync API...
2. Creating producer and producing values...
3. Producing rollout percentage values
   Main: Setting FeatureFlag 0%
[info] [fast] rollout: 0%
[info] [slow] rollout: 0%
   Main: Setting FeatureFlag 25%
[info] [fast] rollout: 25%
   Main: Setting FeatureFlag 50%
[info] [fast] rollout: 50%
[info] [slow] rollout: 50%
   Main: Setting FeatureFlag 75%
[info] [fast] rollout: 75%
   Main: Setting FeatureFlag 100%
[info] [fast] rollout: 100%
[info] [slow] rollout: 100%
4. Shutting down...
Done. The sync producer published rollout updates observed by a SingleLatest tap.
```