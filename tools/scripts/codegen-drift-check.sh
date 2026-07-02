#!/usr/bin/env bash
# Compile aimdb-codegen's generated output against the local workspace.
#
# The codegen templates print API-shaped strings the compiler never checks,
# so they can silently rot against the real AimDB API. This script makes the
# drift loud: it generates a common crate, a hub crate, and a flat schema
# file from tools/codegen-drift/state.toml and `cargo check`s them against
# the workspace at HEAD (design 038 §3.10 decision).
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
STATE="$REPO/tools/codegen-drift/state.toml"
OUT="${CODEGEN_DRIFT_DIR:-$REPO/target/codegen-drift}"

rm -rf "$OUT"
mkdir -p "$OUT/.aimdb" "$OUT/drift-check-flat/src"

run_generate() {
    (cd "$OUT" && cargo run --quiet --manifest-path "$REPO/Cargo.toml" \
        -p aimdb-cli -- generate --state "$STATE" \
        --mermaid "$OUT/.aimdb/architecture.mermaid" "$@")
}

echo "── Generating common crate, hub crate, and flat schema from fixture"
run_generate --common-crate
run_generate --hub
run_generate --rust "$OUT/drift-check-flat/src/generated_schema.rs"

# Wrapper crate around the flat-mode output. The generated Linkable impls
# spell types as `alloc::…` (shared with the no_std common-crate emitters),
# so the wrapper declares `extern crate alloc` like the generated common
# crate's lib.rs does.
cat > "$OUT/drift-check-flat/src/lib.rs" <<'EOF'
extern crate alloc;

mod generated_schema;
pub use generated_schema::*;
EOF
cat > "$OUT/drift-check-flat/Cargo.toml" <<EOF
[package]
name = "drift-check-flat"
version = "0.1.0"
edition = "2021"

[dependencies]
aimdb-core = { path = "$REPO/aimdb-core", features = ["std"] }
aimdb-data-contracts = { path = "$REPO/aimdb-data-contracts", features = ["std", "linkable", "observable"] }
aimdb-tokio-adapter = { path = "$REPO/aimdb-tokio-adapter", features = ["tokio-runtime"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
EOF

# The generated manifests pin crates.io versions; point every aimdb-*
# dependency at the local workspace instead so we compile against HEAD.
sed -i -E "s|^(aimdb-[a-z0-9-]+) = \\{ version = \"[^\"]+\"|\\1 = { path = \"$REPO/\\1\"|" \
    "$OUT/drift-check-common/Cargo.toml" \
    "$OUT/drift-check-hub/Cargo.toml"

# Stitch the three crates into a throwaway workspace and compile.
cat > "$OUT/Cargo.toml" <<'EOF'
[workspace]
members = ["drift-check-common", "drift-check-hub", "drift-check-flat"]
resolver = "2"
EOF

echo "── Compiling generated output against the workspace"
cargo check --manifest-path "$OUT/Cargo.toml" --workspace

echo "✓ codegen output compiles against the workspace"
