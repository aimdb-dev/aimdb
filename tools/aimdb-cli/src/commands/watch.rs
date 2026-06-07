//! Watch Command - Live Record Monitoring

use crate::commands::connect_endpoint;
use crate::error::CliResult;
use crate::output::live;
use clap::Args;
use futures::StreamExt;
use tokio::signal;

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
}

impl WatchCommand {
    pub async fn execute(self, endpoint: Option<&str>) -> CliResult<()> {
        watch_record(
            &self.record,
            endpoint,
            self.queue_size,
            self.count,
            self.full,
        )
        .await
    }
}

async fn watch_record(
    record_name: &str,
    endpoint: Option<&str>,
    queue_size: usize,
    max_count: usize,
    show_full: bool,
) -> CliResult<()> {
    let _ = queue_size; // queue sizing is now an engine concern; kept as a CLI flag
    let conn = connect_endpoint(endpoint).await?;

    // Subscribe to the record (the engine routes updates back by request id; no
    // server-allocated subscription id to track).
    let mut stream = conn.subscribe(record_name)?;

    live::print_watch_start(record_name);

    // Set up Ctrl+C handler
    let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        if signal::ctrl_c().await.is_ok() {
            let _ = cancel_tx.send(());
        }
    });

    // Receive updates. The reshaped wire carries no server sequence, so the
    // watcher counts locally.
    let mut count: u64 = 0;
    let unlimited = max_count == 0;

    loop {
        tokio::select! {
            next = stream.next() => {
                match next {
                    Some(data) => {
                        count += 1;
                        live::print_event(count, &data, show_full);
                        if !unlimited && count >= max_count as u64 {
                            break;
                        }
                    }
                    None => break, // stream ended (record closed or subscribe rejected)
                }
            }
            _ = &mut cancel_rx => break, // Ctrl+C
        }
    }

    // Dropping the stream stops local delivery (no explicit unsubscribe needed).
    live::print_watch_stop();
    Ok(())
}
