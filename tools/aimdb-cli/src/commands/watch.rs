//! Watch Command - Live Record Monitoring

use std::io::Write;
use std::time::Duration;

use crate::commands::connect_endpoint;
use crate::error::{CliError, CliResult, EXIT_ENVIRONMENT, EXIT_REJECTED};
use crate::output::live;
use aimdb_client::watch::{self, WatchOutcome, WatchStatus};
use aimdb_client::RecordUpdate;
use clap::Args;
use tokio::signal;
use tokio::time::Instant;

/// How `watch` prints updates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum WatchFormat {
    /// Human-readable live lines
    #[default]
    Table,
    /// One JSON object per update (NDJSON) on stdout; status goes to stderr
    Json,
}

/// Watch a record for live updates
#[derive(Debug, Args)]
pub struct WatchCommand {
    /// Record name to watch
    pub record: String,

    /// Queue size for subscription
    #[arg(short, long, default_value = "100")]
    pub queue_size: usize,

    /// Maximum number of events to receive (0 = unlimited)
    #[arg(short, long, default_value = "0")]
    pub count: usize,

    /// Show full JSON (pretty-printed) for each event
    #[arg(short, long)]
    pub full: bool,

    /// Output format
    #[arg(long, value_enum, default_value = "table")]
    pub format: WatchFormat,

    /// Only count updates written after the watch is armed (the "Watching
    /// record" line), never the value the record already holds
    #[arg(long)]
    pub new: bool,

    /// Give up after SECS: exit 4 if `--count` updates have not arrived by then
    #[arg(long, value_name = "SECS")]
    pub timeout: Option<u64>,
}

impl WatchCommand {
    pub async fn execute(self, endpoint: Option<&str>) -> CliResult<()> {
        let mut watcher =
            Watcher::new(self.format, self.full, std::io::stdout(), std::io::stderr());
        let options = WatchOptions {
            max_count: self.count,
            new: self.new,
            timeout: self.timeout.map(Duration::from_secs),
        };
        let result = watch_record(&self.record, endpoint, options, &mut watcher).await;
        if let Err(e) = &result {
            watcher.failed(e)?;
        }
        result
    }
}

struct WatchOptions {
    max_count: usize,
    new: bool,
    timeout: Option<Duration>,
}

async fn watch_record<O: Write, E: Write>(
    record_name: &str,
    endpoint: Option<&str>,
    options: WatchOptions,
    watcher: &mut Watcher<O, E>,
) -> CliResult<()> {
    let conn = connect_endpoint(endpoint).await?;

    // The stream is loss-aware, so the watcher reports gaps instead of
    // skipping over them silently. With `--new`, the record's current stamp
    // becomes the baseline, so a value written before the watch never counts.
    let (stream, baseline) = if options.new {
        let armed = watch::arm(&conn, record_name).await?;
        (armed.stream, armed.baseline)
    } else {
        (conn.subscribe(record_name)?, None)
    };

    // Printed once armed: a caller waits for this line before prompting.
    watcher.start(record_name)?;

    // Set up Ctrl+C handler
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        if signal::ctrl_c().await.is_ok() {
            let _ = cancel_tx.send(());
        }
    });

    let deadline = options.timeout.map(|t| Instant::now() + t);
    let waited = tokio::select! {
        outcome = watch::await_updates(stream, baseline, options.max_count, deadline, |u| watcher.update(u)) => Some(outcome?),
        _ = cancel_rx => None, // Ctrl+C
    };

    // Dropping the stream stops local delivery (no explicit unsubscribe needed).
    watcher.stop()?;
    let Some(outcome) = waited else {
        return watcher.stopped(options.max_count).map_err(Into::into);
    };
    watcher.outcome(&outcome)?;
    match outcome.outcome {
        WatchStatus::Ok => Ok(()),
        WatchStatus::Timeout => Err(CliError::Timeout {
            record: record_name.to_string(),
            received: outcome.received,
            wanted: outcome.wanted,
        }),
        WatchStatus::Disconnected => Err(CliError::connection_failed(
            endpoint.unwrap_or("auto-discovered instance"),
            format!(
                "the subscription ended after {} of {} updates",
                outcome.received, outcome.wanted
            ),
        )),
    }
}

/// Renders a subscription. Values go to `out`; status lines (start, gaps,
/// snapshot completion, stop) go to `out` in table mode and to `err` in JSON
/// mode, so `out` stays pure NDJSON there.
struct Watcher<O: Write, E: Write> {
    format: WatchFormat,
    show_full: bool,
    out: O,
    err: E,
    /// The reshaped wire carries no server sequence, so the watcher counts
    /// locally.
    count: u64,
    /// What the engine observed as lost between deliveries, for the summary.
    skipped_total: u64,
    /// Whether the JSON outcome line went out, so a failure adds no second one.
    outcome_written: bool,
}

impl<O: Write, E: Write> Watcher<O, E> {
    fn new(format: WatchFormat, show_full: bool, out: O, err: E) -> Self {
        Self {
            format,
            show_full,
            out,
            err,
            count: 0,
            skipped_total: 0,
            outcome_written: false,
        }
    }

    /// A status line: beside the values in table mode, on `err` in JSON mode.
    fn status(&mut self, line: &str) -> std::io::Result<()> {
        match self.format {
            WatchFormat::Table => writeln!(self.out, "{line}"),
            WatchFormat::Json => writeln!(self.err, "{line}"),
        }
    }

    fn start(&mut self, record_name: &str) -> std::io::Result<()> {
        self.status(&live::format_watch_start(record_name))
    }

    fn update(&mut self, update: &RecordUpdate) -> std::io::Result<()> {
        self.count += 1;
        self.skipped_total += update.skipped;
        match self.format {
            WatchFormat::Table => {
                if update.skipped > 0 {
                    self.status(&live::format_gap(update.skipped))?;
                }
                match &update.value {
                    Some(value) => writeln!(
                        self.out,
                        "{}",
                        live::format_event(self.count, update.ts, value, self.show_full)
                    )?,
                    // The frame counted toward the tally above but carried no
                    // usable value — say so rather than printing nothing.
                    None => self.status(&live::format_undecodable(self.count))?,
                }
            }
            WatchFormat::Json => {
                // Every update is one line, gaps and undecodable values
                // included, so `skipped` and `snapshot_end` stay on the line
                // they describe.
                let line = serde_json::json!({
                    "seq": self.count,
                    "topic": update.topic,
                    "ts": update.ts.as_nanos(),
                    "value": update.value,
                    "skipped": update.skipped,
                    "snapshot_end": update.snapshot_end,
                });
                writeln!(self.out, "{line}")?;
            }
        }
        // The late-join burst is complete; everything after this is a live
        // event. It is the only marker that tells a truncated initial state
        // from a complete one, so it is always reported.
        if update.snapshot_end {
            self.status(&live::format_snapshot_complete(
                self.count,
                self.skipped_total,
            ))?;
        }
        Ok(())
    }

    fn stop(&mut self) -> std::io::Result<()> {
        self.status(&live::format_watch_stop(self.skipped_total))
    }

    /// How the watch ended, as one JSON line on `err` in JSON mode, so a caller
    /// reading the output from files needs no exit code. Table mode leaves it
    /// to the exit code and the error message.
    fn outcome(&mut self, outcome: &WatchOutcome) -> std::io::Result<()> {
        self.outcome_line(serde_json::json!(outcome))
    }

    /// The outcome line for a watch stopped with Ctrl+C.
    fn stopped(&mut self, wanted: usize) -> std::io::Result<()> {
        self.outcome_line(serde_json::json!({
            "outcome": "stopped",
            "received": self.count,
            "wanted": wanted,
            "skipped": self.skipped_total,
        }))
    }

    /// The outcome line for a watch that failed: `refused` when the instance
    /// said no, `disconnected` when it could not be reached.
    fn failed(&mut self, error: &CliError) -> std::io::Result<()> {
        if self.outcome_written {
            return Ok(());
        }
        let outcome = match error.exit_code() {
            EXIT_REJECTED => "refused",
            EXIT_ENVIRONMENT => "disconnected",
            _ => "error",
        };
        self.outcome_line(serde_json::json!({ "outcome": outcome, "error": error.to_string() }))
    }

    fn outcome_line(&mut self, line: serde_json::Value) -> std::io::Result<()> {
        if self.format == WatchFormat::Json {
            writeln!(self.err, "{line}")?;
            self.outcome_written = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aimdb_client::Stamp;
    use serde_json::{json, Value};

    fn update(ts: u64, value: Option<Value>, skipped: u64, snapshot_end: bool) -> RecordUpdate {
        RecordUpdate {
            topic: Some("temp.vienna".into()),
            value,
            ts: Stamp::from_nanos(ts),
            skipped,
            snapshot_end,
        }
    }

    /// `watch --format json`: stdout stays valid NDJSON through startup, a
    /// gap, an undecodable value, snapshot completion and shutdown; every
    /// status line goes to stderr.
    #[test]
    fn json_stdout_is_ndjson_through_a_whole_session() {
        let mut watcher = Watcher::new(WatchFormat::Json, false, Vec::new(), Vec::new());
        watcher.start("temp.vienna").unwrap();
        watcher
            .update(&update(1_000, Some(json!({"c": 21.5})), 0, true))
            .unwrap();
        watcher
            .update(&update(2_000, Some(json!(22)), 3, false))
            .unwrap();
        watcher.update(&update(3_000, None, 0, false)).unwrap();
        watcher.stop().unwrap();
        watcher
            .outcome(&WatchOutcome {
                outcome: WatchStatus::Timeout,
                received: 3,
                wanted: 4,
                skipped: 3,
            })
            .unwrap();
        // The error that follows a timeout adds no second outcome line.
        watcher
            .failed(&CliError::Timeout {
                record: "temp.vienna".into(),
                received: 3,
                wanted: 4,
            })
            .unwrap();

        let out = String::from_utf8(watcher.out).unwrap();
        let lines: Vec<Value> = out
            .lines()
            .map(|line| serde_json::from_str(line).expect("every stdout line is JSON"))
            .collect();
        assert_eq!(lines.len(), 3, "one line per update: {out}");
        assert!(lines.iter().all(Value::is_object));
        assert_eq!(lines[0]["ts"], 1_000, "`ts` is integer nanoseconds");
        assert_eq!(lines[0]["snapshot_end"], true);
        assert_eq!(lines[1]["skipped"], 3);
        assert_eq!(lines[2]["value"], Value::Null);

        let err = String::from_utf8(watcher.err).unwrap();
        assert!(err.contains("Watching record"), "{err}");
        assert!(err.contains("snapshot complete"), "{err}");
        assert!(err.contains("Stopped watching"), "{err}");
        let outcomes: Vec<Value> = err
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        assert_eq!(
            outcomes,
            [json!({"outcome": "timeout", "received": 3, "wanted": 4, "skipped": 3})],
            "one outcome line, last on stderr: {err}"
        );
        assert!(err.trim_end().ends_with('}'), "{err}");
    }

    /// A failure before any outcome reports `refused` or `disconnected` by its
    /// failure class.
    #[test]
    fn failures_get_an_outcome_line() {
        let mut watcher = Watcher::new(WatchFormat::Json, false, Vec::new(), Vec::new());
        watcher
            .failed(&CliError::server_error("not_found", "no such record", None))
            .unwrap();
        let line: Value = serde_json::from_slice(&watcher.err).unwrap();
        assert_eq!(line["outcome"], "refused");

        let mut watcher = Watcher::new(WatchFormat::Json, false, Vec::new(), Vec::new());
        watcher
            .failed(&CliError::connection_failed("unix:///x", "refused"))
            .unwrap();
        let line: Value = serde_json::from_slice(&watcher.err).unwrap();
        assert_eq!(line["outcome"], "disconnected");

        // Table mode leaves it to the error message and exit code.
        let mut watcher = Watcher::new(WatchFormat::Table, false, Vec::new(), Vec::new());
        watcher
            .failed(&CliError::connection_failed("unix:///x", "refused"))
            .unwrap();
        assert!(watcher.out.is_empty() && watcher.err.is_empty());
    }

    /// Table mode keeps everything on stdout and shows `ts` in seconds.
    #[test]
    fn table_shows_ts_in_seconds() {
        let mut watcher = Watcher::new(WatchFormat::Table, false, Vec::new(), Vec::new());
        watcher
            .update(&update(81_234_567_890, Some(json!(1)), 0, false))
            .unwrap();
        let out = String::from_utf8(watcher.out).unwrap();
        assert!(out.contains("ts:81.234567"), "{out}");
        assert!(watcher.err.is_empty());
    }

    #[test]
    fn format_flag_parses() {
        use clap::Parser;
        #[derive(Parser)]
        struct Cli {
            #[command(flatten)]
            watch: WatchCommand,
        }
        let cli = Cli::parse_from(["aimdb", "temp", "--format", "json"]);
        assert_eq!(cli.watch.format, WatchFormat::Json);
        let cli = Cli::parse_from(["aimdb", "temp"]);
        assert_eq!(cli.watch.format, WatchFormat::Table);
        assert!(!cli.watch.new);
        assert_eq!(cli.watch.timeout, None);
        let cli = Cli::parse_from(["aimdb", "temp", "--new", "--timeout", "60"]);
        assert!(cli.watch.new);
        assert_eq!(cli.watch.timeout, Some(60));
    }
}
