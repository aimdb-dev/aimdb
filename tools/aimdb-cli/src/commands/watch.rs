//! Watch Command - Live Record Monitoring

use std::io::Write;

use crate::commands::connect_endpoint;
use crate::error::CliResult;
use crate::output::live;
use aimdb_client::RecordUpdate;
use clap::Args;
use futures::StreamExt;
use tokio::signal;

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
}

impl WatchCommand {
    pub async fn execute(self, endpoint: Option<&str>) -> CliResult<()> {
        watch_record(
            &self.record,
            endpoint,
            self.queue_size,
            self.count,
            Watcher::new(self.format, self.full, std::io::stdout(), std::io::stderr()),
        )
        .await
    }
}

async fn watch_record<O: Write, E: Write>(
    record_name: &str,
    endpoint: Option<&str>,
    queue_size: usize,
    max_count: usize,
    mut watcher: Watcher<O, E>,
) -> CliResult<()> {
    let _ = queue_size; // queue sizing is now an engine concern; kept as a CLI flag
    let conn = connect_endpoint(endpoint).await?;

    // Subscribe to the record (the engine routes updates back by request id; no
    // server-allocated subscription id to track). The stream is loss-aware, so
    // the watcher reports gaps instead of skipping over them silently.
    let mut stream = conn.subscribe(record_name)?;

    watcher.start(record_name)?;

    // Set up Ctrl+C handler
    let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        if signal::ctrl_c().await.is_ok() {
            let _ = cancel_tx.send(());
        }
    });

    let unlimited = max_count == 0;
    loop {
        tokio::select! {
            next = stream.next() => {
                match next {
                    Some(Ok(update)) => {
                        watcher.update(&update)?;
                        if !unlimited && watcher.count >= max_count as u64 {
                            break;
                        }
                    }
                    // A refusal is terminal: report it and exit non-zero rather
                    // than closing out as if the session had run.
                    Some(Err(e)) => return Err(e.into()),
                    None => break, // stream ended (record closed, or disconnect)
                }
            }
            _ = &mut cancel_rx => break, // Ctrl+C
        }
    }

    // Dropping the stream stops local delivery (no explicit unsubscribe needed).
    watcher.stop()?;
    Ok(())
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
    }
}
