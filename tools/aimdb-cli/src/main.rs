//! AimDB CLI - Command-line interface for AimDB introspection and management
//!
//! This tool provides commands to discover, inspect, and interact with running
//! AimDB instances via the AimX v1 remote access protocol.

use clap::{Parser, Subcommand};
use commands::{instance::InstanceCommand, record::RecordCommand, watch::WatchCommand};

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

    /// Watch a record in real-time
    #[command(name = "watch")]
    Watch(WatchCommand),
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::Instance(cmd) => cmd.execute().await,
        Command::Record(cmd) => cmd.execute().await,
        Command::Watch(cmd) => cmd.execute().await,
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}
