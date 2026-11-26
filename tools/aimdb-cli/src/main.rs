//! AimDB CLI - Command-line interface for AimDB introspection and management
//!
//! This tool provides commands to discover, inspect, and interact with running
//! AimDB instances via the AimX remote access protocol (v2 NDJSON wire).

use std::time::Duration;

use clap::{Parser, Subcommand};
use commands::{
    graph::GraphCommand, instance::InstanceCommand, record::RecordCommand, watch::WatchCommand,
    Target,
};

mod commands;
mod error;
mod output;

/// AimDB CLI - Introspect and manage running AimDB instances
#[derive(Debug, Parser)]
#[command(name = "aimdb")]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Endpoint of the AimDB instance: a `scheme://` URL (`unix://PATH`,
    /// `serial://DEVICE?baud=N`, `tcp://HOST:PORT`) or a bare path (the
    /// `unix://` shorthand).
    /// Falls back to the `AIMDB_CONNECT` env var, then auto-discovery.
    #[arg(long, global = true)]
    connect: Option<String>,

    /// Seconds to wait for each reply from the instance before failing with
    /// exit code 3. Covers requests after the handshake (`record get/set`,
    /// `graph …`, `instance info`), not the updates `watch` waits for: that
    /// has its own `--timeout`.
    #[arg(
        long,
        global = true,
        value_name = "SECS",
        default_value_t = aimdb_client::engine::DEFAULT_REQUEST_TIMEOUT.as_secs(),
        value_parser = clap::value_parser!(u64).range(1..),
    )]
    request_timeout: u64,

    /// Enable verbose output
    #[arg(short, long, global = true)]
    verbose: bool,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Instance management commands
    #[command(name = "instance")]
    Instance(InstanceCommand),

    /// Record management commands
    #[command(name = "record")]
    Record(RecordCommand),

    /// Graph introspection commands
    #[command(name = "graph")]
    Graph(GraphCommand),

    /// Watch a record in real-time
    #[command(name = "watch")]
    Watch(WatchCommand),
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Endpoint precedence: --connect, then AIMDB_CONNECT, else auto-discovery (None).
    let endpoint = cli
        .connect
        .or_else(|| std::env::var("AIMDB_CONNECT").ok())
        .filter(|s| !s.is_empty());
    let target = Target {
        endpoint: endpoint.as_deref(),
        request_timeout: Duration::from_secs(cli.request_timeout),
    };

    let result = match cli.command {
        Command::Instance(cmd) => cmd.execute(target).await,
        Command::Record(cmd) => cmd.execute(target).await,
        Command::Graph(cmd) => cmd.execute(target).await,
        Command::Watch(cmd) => cmd.execute(target).await,
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        std::process::exit(e.exit_code());
    }
}
