# aimdb-codegen (discontinued)

This crate is discontinued, and 0.4.1 is its last release. It will receive no
further updates.

`aimdb-codegen` read an `.aimdb/state.toml` architecture model and printed
Mermaid diagrams and Rust source. The `state.toml` model predates the current
AimDB API and cannot express it, and the compiler never checked the generated
code. The crate, the `aimdb generate` command and the architecture-agent tools in
`aimdb-mcp` were removed together
([aimdb#411](https://github.com/aimdb-dev/aimdb/issues/411)).

## What to use instead

- **Write the wiring in Rust**, using `reg.input` / `reg.output` and
  `builder.connect`, as the
  [AimDB usage guide](https://github.com/aimdb-dev/aimdb/blob/main/docs/aimdb-usage-guide.md)
  shows.
- **Check it with the compiler.** Mistakes in the wiring are compile errors that
  name the fix.
- **Inspect the running result** with
  [`aimdb-mcp`](https://crates.io/crates/aimdb-mcp) or `aimdb watch` from
  [`aimdb-cli`](https://crates.io/crates/aimdb-cli).

## License

Apache-2.0, as the rest of the AimDB workspace.
