//! Watch Command - Real-time Record Monitoring

use crate::client::connection::AimxClient;
use crate::client::discovery::find_instance;
use crate::error::CliResult;
use crate::output::live;
use clap::Args;
use tokio::signal;

/// Watch a record in real-time
#[derive(Debug, Args)]
pub struct WatchCommand {
    /// Record name to watch
    pub record: String,

    /// Socket path (optional, uses auto-discovery if not specified)
    #[arg(short, long)]
    pub socket: Option<String>,

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
    pub async fn execute(self) -> CliResult<()> {
        watch_record(
            &self.record,
            self.socket.as_deref(),
            self.queue_size,
            self.count,
            self.full,
        )
        .await
    }
}

async fn watch_record(
    record_name: &str,
    socket: Option<&str>,
    queue_size: usize,
    max_count: usize,
    show_full: bool,
) -> CliResult<()> {
    let instance = find_instance(socket).await?;
    let mut client = AimxClient::connect(&instance.socket_path).await?;

    // Subscribe to record
    let subscription_id = client.subscribe(record_name, queue_size).await?;

    // Print start message
    live::print_watch_start(record_name, &subscription_id);

    // Set up Ctrl+C handler
    let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        if signal::ctrl_c().await.is_ok() {
            let _ = cancel_tx.send(());
        }
    });

    // Receive events
    let mut count = 0;
    let unlimited = max_count == 0;

    loop {
        tokio::select! {
            event = client.receive_event() => {
                let event = event?;

                // Only show events for this subscription
                if event.subscription_id == subscription_id {
                    live::print_event(&event, show_full);

                    count += 1;
                    if !unlimited && count >= max_count {
                        break;
                    }
                }
            }
            _ = &mut cancel_rx => {
                // User pressed Ctrl+C
                break;
            }
        }
    }

    // Unsubscribe cleanly
    client.unsubscribe(&subscription_id).await?;

    live::print_watch_stop();

    Ok(())
}
