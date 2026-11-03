//! Arm a watch on a record, then wait for updates written after it was armed.
//!
//! The split lets a caller do something in between, such as asking a person to
//! press a button, without missing an update that arrives meanwhile:
//!
//! ```no_run
//! # async fn demo(conn: &aimdb_client::AimxConnection) -> Result<(), aimdb_client::ClientError> {
//! use std::time::Duration;
//! use aimdb_client::watch::{arm, await_updates};
//!
//! let armed = arm(conn, "button.b1").await?;
//! // Prompt now: from here on, every update is caught.
//! let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
//! let outcome = await_updates(armed.stream, armed.baseline, 1, Some(deadline), |u| {
//!     println!("{:?}", u.value);
//!     Ok(())
//! })
//! .await?;
//! println!("{}", serde_json::to_string(&outcome).unwrap());
//! # Ok(())
//! # }
//! ```

use futures::{stream, FutureExt, Stream, StreamExt};
use serde::Serialize;
use tokio::time::Instant;

use aimdb_core::session::BoxStream;
use aimdb_core::Stamp;

use crate::engine::{AimxConnection, RecordUpdate};
use crate::error::ClientError;

/// A subscription opened by [`arm`], with the write stamp of the value the
/// record held when it was armed.
pub struct Armed {
    /// The record's update stream.
    pub stream: BoxStream<'static, Result<RecordUpdate, ClientError>>,
    /// Stamp of the record's value at arm time; `None` if it had none. Pass it
    /// to [`await_updates`], so a value from before the watch never counts.
    pub baseline: Option<Stamp>,
}

/// How a watch ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WatchStatus {
    /// `count` updates arrived; with `count == 0`, the deadline passed or the
    /// stream ended.
    Ok,
    /// The deadline passed before `count` updates arrived.
    Timeout,
    /// The stream ended before `count` updates arrived (connection lost or the
    /// record closed). Stamps are only comparable within one instance
    /// lifetime, so the watch is not resumed.
    Disconnected,
}

/// The result of [`await_updates`]. Serializes as
/// `{"outcome":"ok","received":1,"wanted":1,"skipped":0}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct WatchOutcome {
    pub outcome: WatchStatus,
    /// Updates that passed the baseline filter.
    pub received: u64,
    /// The `count` asked for; `0` means unlimited.
    pub wanted: usize,
    /// Updates the server reported as lost among those received.
    pub skipped: u64,
}

/// Subscribe to `record` and take its current write stamp as the baseline.
///
/// The subscribe is sent before the `record.get`, and both sides keep that
/// order, so no write falls between the baseline and the subscription. A
/// refused subscribe (unknown record, denied, over the per-connection limit)
/// arrives before the get's reply and is returned as `Err` here.
///
/// "Armed" means subscribed and baseline taken, not "guaranteed to deliver": a
/// failure the server reports later, such as a buffer out of subscriber slots,
/// still ends the stream with an `Err`.
pub async fn arm(conn: &AimxConnection, record: &str) -> Result<Armed, ClientError> {
    let mut stream = conn.subscribe(record)?;
    let baseline = match conn.get_record(record).await {
        Ok(current) => Some(current.stamp()),
        // No value yet (or an unknown record, whose subscribe refusal is
        // picked up below).
        Err(ClientError::ServerError { code, .. }) if code == "not_found" => None,
        Err(e) => return Err(e),
    };
    let stream = match stream.next().now_or_never() {
        Some(Some(Err(e))) => return Err(e),
        // An update that already arrived goes back in front, so the baseline
        // filter sees it.
        Some(Some(Ok(update))) => Box::pin(stream::iter(Some(Ok(update))).chain(stream)),
        Some(None) | None => stream,
    };
    Ok(Armed { stream, baseline })
}

/// Wait until `count` updates newer than `baseline` arrived, the `deadline`
/// passed, or the stream ended. `count == 0` waits for the deadline or the end
/// of the stream.
///
/// Updates stamped at or before `baseline` are dropped; every other update is
/// handed to `on_update` as it arrives. An `Err` item (a refusal) is returned
/// as `Err`, and so is an error from `on_update`.
pub async fn await_updates<S, F>(
    mut stream: S,
    baseline: Option<Stamp>,
    count: usize,
    deadline: Option<Instant>,
    mut on_update: F,
) -> Result<WatchOutcome, ClientError>
where
    S: Stream<Item = Result<RecordUpdate, ClientError>> + Unpin,
    F: FnMut(&RecordUpdate) -> std::io::Result<()>,
{
    let mut outcome = WatchOutcome {
        outcome: WatchStatus::Ok,
        received: 0,
        wanted: count,
        skipped: 0,
    };
    // Unmet only while a positive `count` is still outstanding.
    let pending = |o: &WatchOutcome| count > 0 && o.received < count as u64;
    while count == 0 || pending(&outcome) {
        let next = match deadline {
            Some(at) => match tokio::time::timeout_at(at, stream.next()).await {
                Ok(next) => next,
                Err(_) => {
                    if pending(&outcome) {
                        outcome.outcome = WatchStatus::Timeout;
                    }
                    return Ok(outcome);
                }
            },
            None => stream.next().await,
        };
        match next {
            Some(Ok(update)) => {
                if baseline.is_some_and(|b| update.ts <= b) {
                    continue;
                }
                on_update(&update)?;
                outcome.received += 1;
                outcome.skipped += update.skipped;
            }
            Some(Err(e)) => return Err(e),
            None => {
                if pending(&outcome) {
                    outcome.outcome = WatchStatus::Disconnected;
                }
                return Ok(outcome);
            }
        }
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    fn update(ts: u64) -> Result<RecordUpdate, ClientError> {
        Ok(RecordUpdate {
            topic: None,
            value: Some(json!(ts)),
            ts: Stamp::from_nanos(ts),
            skipped: 0,
            snapshot_end: false,
        })
    }

    /// Runs `await_updates` and collects the stamps `on_update` saw.
    async fn run<S>(
        stream: S,
        baseline: Option<u64>,
        count: usize,
        timeout: Option<Duration>,
    ) -> (Result<WatchOutcome, ClientError>, Vec<u64>)
    where
        S: Stream<Item = Result<RecordUpdate, ClientError>> + Unpin,
    {
        let mut seen = Vec::new();
        let deadline = timeout.map(|t| Instant::now() + t);
        let result = await_updates(
            stream,
            baseline.map(Stamp::from_nanos),
            count,
            deadline,
            |u| {
                seen.push(u.ts.as_nanos());
                Ok(())
            },
        )
        .await;
        (result, seen)
    }

    #[tokio::test(start_paused = true)]
    async fn baseline_drops_equal_and_older_stamps() {
        let s = stream::iter([update(5), update(10), update(11), update(12)]);
        let (result, seen) = run(s, Some(10), 2, None).await;
        assert_eq!(result.unwrap().outcome, WatchStatus::Ok);
        assert_eq!(seen, [11, 12]);
    }

    #[tokio::test(start_paused = true)]
    async fn no_baseline_keeps_every_update() {
        let s = stream::iter([update(1), update(2)]);
        let (result, seen) = run(s, None, 2, None).await;
        let outcome = result.unwrap();
        assert_eq!(outcome.outcome, WatchStatus::Ok);
        assert_eq!(outcome.received, 2);
        assert_eq!(seen, [1, 2]);
    }

    #[tokio::test(start_paused = true)]
    async fn stops_once_count_is_reached() {
        let s = stream::iter([update(1), update(2), update(3)]);
        let (result, seen) = run(s, None, 2, None).await;
        assert_eq!(result.unwrap().outcome, WatchStatus::Ok);
        assert_eq!(seen, [1, 2], "the third update is never taken");
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_before_count_is_a_timeout_with_the_partial_updates() {
        let s = stream::iter([update(1)]).chain(stream::pending());
        let (result, seen) = run(s, None, 3, Some(Duration::from_secs(5))).await;
        let outcome = result.unwrap();
        assert_eq!(outcome.outcome, WatchStatus::Timeout);
        assert_eq!((outcome.received, outcome.wanted), (1, 3));
        assert_eq!(seen, [1]);
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_with_count_zero_is_ok() {
        let s = stream::iter([update(1)]).chain(stream::pending());
        let (result, seen) = run(s, None, 0, Some(Duration::from_secs(5))).await;
        assert_eq!(result.unwrap().outcome, WatchStatus::Ok);
        assert_eq!(seen, [1]);
    }

    #[tokio::test(start_paused = true)]
    async fn stream_end_before_count_is_disconnected() {
        let (result, _) = run(stream::iter([update(1)]), None, 2, None).await;
        assert_eq!(result.unwrap().outcome, WatchStatus::Disconnected);
    }

    #[tokio::test(start_paused = true)]
    async fn stream_end_with_count_zero_is_ok() {
        let (result, seen) = run(stream::iter([update(1)]), None, 0, None).await;
        assert_eq!(result.unwrap().outcome, WatchStatus::Ok);
        assert_eq!(seen, [1]);
    }

    #[tokio::test(start_paused = true)]
    async fn an_err_item_is_passed_through() {
        let refusal = Err(ClientError::server_error("denied", "no", None));
        let (result, _) = run(stream::iter([refusal]), None, 1, None).await;
        assert!(
            matches!(result, Err(ClientError::ServerError { ref code, .. }) if code == "denied")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn skipped_is_summed_over_received_updates() {
        let mut gap = update(2).unwrap();
        gap.skipped = 4;
        let s = stream::iter([update(1), Ok(gap)]);
        let (result, _) = run(s, None, 2, None).await;
        assert_eq!(result.unwrap().skipped, 4);
    }

    #[test]
    fn outcome_serializes_to_the_documented_json() {
        let outcome = WatchOutcome {
            outcome: WatchStatus::Timeout,
            received: 0,
            wanted: 1,
            skipped: 0,
        };
        assert_eq!(
            serde_json::to_value(outcome).unwrap(),
            json!({"outcome": "timeout", "received": 0, "wanted": 1, "skipped": 0})
        );
        assert_eq!(
            serde_json::to_value(WatchStatus::Disconnected).unwrap(),
            json!("disconnected")
        );
    }
}
