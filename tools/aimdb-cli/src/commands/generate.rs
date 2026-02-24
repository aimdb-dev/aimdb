//! `aimdb generate` subcommand
//!
//! Reads `.aimdb/state.toml` and emits:
//! - `.aimdb/architecture.mermaid` — Mermaid diagram
//! - `src/generated_schema.rs` — compilable Rust schema
//!
//! # Usage
//!
//! ```text
//! aimdb generate                          # generate both artefacts
//! aimdb generate --check                  # validate only (CI)
//! aimdb generate --dry-run                # print to stdout, don't write
//! aimdb generate --state path/state.toml  # custom state path
//! ```
//!
//! Integrate with cargo-watch:
//! ```text
//! cargo watch -s "aimdb generate && cargo check"
//! ```

use crate::error::CliResult;
use aimdb_codegen::{generate_mermaid, generate_rust, validate, ArchitectureState, Severity};
use anyhow::Context;
use clap::Args;
use colored::Colorize;
use std::path::{Path, PathBuf};

/// Generate Mermaid diagram and Rust schema from `.aimdb/state.toml`
#[derive(Debug, Args)]
pub struct GenerateCommand {
    /// Path to state.toml
    #[arg(long, default_value = ".aimdb/state.toml")]
    pub state: PathBuf,

    /// Output path for Mermaid diagram
    #[arg(long, default_value = ".aimdb/architecture.mermaid")]
    pub mermaid: PathBuf,

    /// Output path for generated Rust source
    #[arg(long, default_value = "src/generated_schema.rs")]
    pub rust: PathBuf,

    /// Validate state.toml without writing files (exit 1 if errors found)
    #[arg(long)]
    pub check: bool,

    /// Print generated output to stdout instead of writing files
    #[arg(long)]
    pub dry_run: bool,
}

impl GenerateCommand {
    pub async fn execute(self) -> CliResult<()> {
        // ── Read state.toml ────────────────────────────────────────────────
        let state_path = &self.state;
        if !state_path.exists() {
            return Err(anyhow::anyhow!(
                "state file not found: {}\n\
                 Hint: start an architecture session with the AimDB architecture agent,\n\
                 or create .aimdb/state.toml manually.",
                state_path.display()
            )
            .into());
        }

        let toml_src = std::fs::read_to_string(state_path)
            .with_context(|| format!("reading {}", state_path.display()))?;

        let state = ArchitectureState::from_toml(&toml_src)
            .with_context(|| format!("parsing {}", state_path.display()))?;

        // ── Validate ───────────────────────────────────────────────────────
        let errors = validate(&state);
        let has_errors = print_validation_results(&errors, state_path);

        if has_errors {
            return Err(
                anyhow::anyhow!("validation failed — fix the errors above and retry").into(),
            );
        }

        if self.check {
            println!(
                "{} {} validated successfully",
                "✓".green(),
                state_path.display()
            );
            return Ok(());
        }

        // ── Generate ───────────────────────────────────────────────────────
        let mermaid_src = generate_mermaid(&state);
        let rust_src = generate_rust(&state);

        if self.dry_run {
            println!("{}  {}", "── Mermaid".dimmed(), self.mermaid.display());
            println!("{mermaid_src}");
            println!("{}  {}", "── Rust".dimmed(), self.rust.display());
            println!("{rust_src}");
            return Ok(());
        }

        // ── Write files ────────────────────────────────────────────────────
        write_if_changed(&self.mermaid, &mermaid_src, "Mermaid")?;
        write_if_changed(&self.rust, &rust_src, "Rust")?;

        println!(
            "{} {} record(s) processed",
            "✓".green(),
            state.records.len()
        );

        Ok(())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Print validation results and return `true` if any errors were found.
fn print_validation_results(errors: &[aimdb_codegen::ValidationError], state_path: &Path) -> bool {
    let mut has_errors = false;
    for e in errors {
        match e.severity {
            Severity::Error => {
                eprintln!("{} [{}] {}", "✗".red(), e.location, e.message);
                has_errors = true;
            }
            Severity::Warning => {
                eprintln!("{} [{}] {}", "!".yellow(), e.location, e.message);
            }
        }
    }
    if !errors.is_empty() {
        eprintln!("  in: {}", state_path.display());
    }
    has_errors
}

/// Write `contents` to `path`, creating parent directories as needed.
/// Prints a status line indicating whether the file was created or unchanged.
fn write_if_changed(path: &Path, contents: &str, label: &str) -> CliResult<()> {
    // Create parent directory if needed
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating directory: {}", parent.display()))?;
    }

    // Read existing to detect changes
    let existing = std::fs::read_to_string(path).ok();
    let changed = existing.as_deref() != Some(contents);

    if changed {
        std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
        println!("  {} {} written", "→".cyan(), path.display());
    } else {
        println!("  {} {} unchanged", "·".dimmed(), path.display());
    }

    let _ = label; // Suppresses unused warning; label available for future verbose output
    Ok(())
}
