//! `aimdb generate` subcommand
//!
//! Reads `.aimdb/state.toml` and emits:
//! - `.aimdb/architecture.mermaid` — Mermaid diagram
//! - `src/generated_schema.rs` — compilable Rust schema (flat mode)
//! - `{project.name}-common/` — compilable common crate (`--common-crate` mode)
//!
//! # Usage
//!
//! ```text
//! aimdb generate                          # generate flat file + diagram
//! aimdb generate --common-crate           # generate common crate directory
//! aimdb generate --hub                    # generate hub binary crate scaffold
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
use aimdb_codegen::{
    generate_cargo_toml, generate_hub_cargo_toml, generate_hub_main_rs, generate_hub_tasks_rs,
    generate_lib_rs, generate_mermaid, generate_rust, generate_schema_rs, validate,
    ArchitectureState, Severity,
};
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

    /// Output path for generated Rust source (flat mode only)
    #[arg(long, default_value = "src/generated_schema.rs")]
    pub rust: PathBuf,

    /// Generate a complete common crate directory instead of a flat file.
    /// Requires `[project]` block in state.toml. Outputs to `{project.name}-common/`.
    #[arg(long)]
    pub common_crate: bool,

    /// Generate a complete hub binary crate directory.
    /// Requires `[project]` block in state.toml. Outputs to `{project.name}-hub/`.
    /// `src/tasks.rs` is only written if it does not already exist.
    #[arg(long)]
    pub hub: bool,

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

        if self.hub {
            self.execute_hub_crate(&state).await
        } else if self.common_crate {
            self.execute_common_crate(&state).await
        } else {
            self.execute_flat(&state).await
        }
    }

    /// Flat mode: generate `src/generated_schema.rs` and `.aimdb/architecture.mermaid`.
    async fn execute_flat(&self, state: &ArchitectureState) -> CliResult<()> {
        let mermaid_src = generate_mermaid(state);
        let rust_src = generate_rust(state);

        if self.dry_run {
            println!("{}  {}", "── Mermaid".dimmed(), self.mermaid.display());
            println!("{mermaid_src}");
            println!("{}  {}", "── Rust".dimmed(), self.rust.display());
            println!("{rust_src}");
            return Ok(());
        }

        write_if_changed(&self.mermaid, &mermaid_src, "Mermaid")?;
        write_if_changed(&self.rust, &rust_src, "Rust")?;

        println!(
            "{} {} record(s) processed",
            "✓".green(),
            state.records.len()
        );

        Ok(())
    }

    /// Hub crate mode: generate `{project.name}-hub/` binary crate.
    async fn execute_hub_crate(&self, state: &ArchitectureState) -> CliResult<()> {
        let project = state.project.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "--hub requires a [project] block in state.toml.\n\
                 Add:\n  [project]\n  name = \"your-project\""
            )
        })?;

        let hub_dir = PathBuf::from(format!("{}-hub", project.name));
        let src_dir = hub_dir.join("src");

        let cargo_toml = generate_hub_cargo_toml(state);
        let main_rs = generate_hub_main_rs(state);
        let tasks_rs = generate_hub_tasks_rs(state);

        if self.dry_run {
            println!(
                "{}  {}",
                "── Cargo.toml".dimmed(),
                hub_dir.join("Cargo.toml").display()
            );
            println!("{cargo_toml}");
            println!(
                "{}  {}",
                "── main.rs".dimmed(),
                src_dir.join("main.rs").display()
            );
            println!("{main_rs}");
            println!(
                "{}  {}",
                "── tasks.rs".dimmed(),
                src_dir.join("tasks.rs").display()
            );
            println!("{tasks_rs}");
            return Ok(());
        }

        // Also regenerate the Mermaid diagram
        let mermaid_src = generate_mermaid(state);
        write_if_changed(&self.mermaid, &mermaid_src, "Mermaid")?;

        write_if_changed(&hub_dir.join("Cargo.toml"), &cargo_toml, "Cargo.toml")?;
        write_if_changed(&src_dir.join("main.rs"), &main_rs, "main.rs")?;

        // tasks.rs is scaffolded once — never overwritten
        let tasks_path = src_dir.join("tasks.rs");
        if !tasks_path.exists() {
            write_if_changed(&tasks_path, &tasks_rs, "tasks.rs")?;
        } else {
            println!(
                "  {} {} (user-owned, skipped)",
                "·".dimmed(),
                tasks_path.display()
            );
        }

        println!(
            "{} hub crate {} generated ({} record(s))",
            "✓".green(),
            hub_dir.display(),
            state.records.len()
        );

        Ok(())
    }

    /// Common crate mode: generate `{project.name}-common/` directory.
    async fn execute_common_crate(&self, state: &ArchitectureState) -> CliResult<()> {
        let project = state.project.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "--common-crate requires a [project] block in state.toml.\n\
                 Add:\n  [project]\n  name = \"your-project\""
            )
        })?;

        let crate_dir = PathBuf::from(format!("{}-common", project.name));
        let src_dir = crate_dir.join("src");

        let cargo_toml = generate_cargo_toml(state);
        let lib_rs = generate_lib_rs();
        let schema_rs = generate_schema_rs(state);

        if self.dry_run {
            let cargo_path = crate_dir.join("Cargo.toml");
            let lib_path = src_dir.join("lib.rs");
            let schema_path = src_dir.join("schema.rs");

            println!("{}  {}", "── Cargo.toml".dimmed(), cargo_path.display());
            println!("{cargo_toml}");
            println!("{}  {}", "── lib.rs".dimmed(), lib_path.display());
            println!("{lib_rs}");
            println!("{}  {}", "── schema.rs".dimmed(), schema_path.display());
            println!("{schema_rs}");
            return Ok(());
        }

        // Also generate the Mermaid diagram
        let mermaid_src = generate_mermaid(state);
        write_if_changed(&self.mermaid, &mermaid_src, "Mermaid")?;

        // Write common crate files
        write_if_changed(&crate_dir.join("Cargo.toml"), &cargo_toml, "Cargo.toml")?;
        write_if_changed(&src_dir.join("lib.rs"), &lib_rs, "lib.rs")?;
        write_if_changed(&src_dir.join("schema.rs"), &schema_rs, "schema.rs")?;

        println!(
            "{} common crate {} generated ({} record(s))",
            "✓".green(),
            crate_dir.display(),
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
