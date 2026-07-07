//! AimDB CLI - Command-line interface for AimDB introspection and management
//!
//! This tool provides commands to discover, inspect, and interact with running
//! AimDB instances via the AimX remote access protocol (v2 NDJSON wire).

use clap::{Parser, Subcommand};
use commands::{
    generate::GenerateCommand, graph::GraphCommand, instance::InstanceCommand,
    record::RecordCommand, watch::WatchCommand,
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

    /// Generate architecture artefacts from state.toml
    #[command(name = "generate")]
    Generate(GenerateCommand),
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Endpoint precedence: --connect, then AIMDB_CONNECT, else auto-discovery (None).
    let endpoint = cli
        .connect
        .or_else(|| std::env::var("AIMDB_CONNECT").ok())
        .filter(|s| !s.is_empty());
    let endpoint = endpoint.as_deref();

    let result = match cli.command {
        Command::Instance(cmd) => cmd.execute(endpoint).await,
        Command::Record(cmd) => cmd.execute(endpoint).await,
        Command::Graph(cmd) => cmd.execute(endpoint).await,
        Command::Watch(cmd) => cmd.execute(endpoint).await,
        Command::Generate(cmd) => cmd.execute().await,
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}
