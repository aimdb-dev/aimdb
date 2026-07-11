#!/usr/bin/env bash
# Compile aimdb-codegen's generated output against the local workspace.
#
# The codegen templates print API-shaped strings the compiler never checks,
# so they can silently rot against the real AimDB API. This script makes the
# drift loud: it generates the default common/hub/flat outputs plus Postcard-
# only and mixed-codec common crates, then compiles and exercises them against
# the workspace at HEAD (design 038 §3.10 decision, issue #155).
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
STATE="$REPO/tools/codegen-drift/state.toml"
POSTCARD_STATE="$REPO/tools/codegen-drift/state-postcard.toml"
MIXED_CODEC_STATE="$REPO/tools/codegen-drift/state-mixed-codec.toml"
OUT="${CODEGEN_DRIFT_DIR:-$REPO/target/codegen-drift}"

rm -rf "$OUT"
mkdir -p "$OUT/.aimdb" "$OUT/drift-check-flat/src"

run_generate() {
    local state="$1"
    shift
    (cd "$OUT" && cargo run --quiet --manifest-path "$REPO/Cargo.toml" \
        -p aimdb-cli -- generate --state "$state" \
        --mermaid "$OUT/.aimdb/architecture.mermaid" "$@")
}

echo "── Generating common crate, hub crate, and flat schema from fixture"
run_generate "$STATE" --common-crate
run_generate "$STATE" --hub
run_generate "$STATE" --rust "$OUT/drift-check-flat/src/generated_schema.rs"

echo "── Generating Postcard-only and mixed-codec common crates"
run_generate "$POSTCARD_STATE" --common-crate
run_generate "$MIXED_CODEC_STATE" --common-crate

# Exercise the generated Postcard codec as behavior, not just compilable tokens.
mkdir -p "$OUT/postcard-drift-common/tests"
cat > "$OUT/postcard-drift-common/tests/postcard_roundtrip.rs" <<'EOF'
use aimdb_data_contracts::Linkable;
use postcard_drift_common::PostcardReadingValue;

#[test]
fn generated_postcard_linkable_roundtrips() {
    let expected = PostcardReadingValue {
        value: 23.75,
        sequence: 42,
    };

    let bytes = expected.to_bytes().expect("serialize with postcard");
    assert!(!bytes.is_empty());

    let actual = PostcardReadingValue::from_bytes(&bytes).expect("deserialize with postcard");
    assert_eq!(actual.value.to_bits(), expected.value.to_bits());
    assert_eq!(actual.sequence, expected.sequence);
}
EOF

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

# The flat-mode Linkable impls are gated on `#[cfg(feature = "std")]` (shared
# with the no_std common-crate emitters), so this wrapper crate declares its
# own "std" feature to match — otherwise rustc's check-cfg lint flags the
# generated code as referencing an unknown feature.
[features]
default = ["std"]
std = []

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
    "$OUT/drift-check-hub/Cargo.toml" \
    "$OUT/postcard-drift-common/Cargo.toml" \
    "$OUT/mixed-codec-drift-common/Cargo.toml"

# Stitch the generated crates into a throwaway workspace and compile.
cat > "$OUT/Cargo.toml" <<'EOF'
[workspace]
members = [
    "drift-check-common",
    "drift-check-hub",
    "drift-check-flat",
    "postcard-drift-common",
    "mixed-codec-drift-common",
]
resolver = "2"
EOF

echo "── Compiling generated output against the workspace"
cargo check --manifest-path "$OUT/Cargo.toml" --workspace

echo "── Running generated Postcard Linkable roundtrip"
cargo test --manifest-path "$OUT/Cargo.toml" -p postcard-drift-common

echo "── Cross-compiling generated Postcard crate for Cortex-M"
cargo check --manifest-path "$OUT/Cargo.toml" -p postcard-drift-common \
    --target thumbv7em-none-eabihf \
    --target-dir "$OUT/target-thumb" \
    --no-default-features --features alloc

assert_not_in_normal_graph() {
    local package="$1"
    local dependency="$2"
    local output

    if ! output=$(cargo tree --manifest-path "$OUT/Cargo.toml" \
        -p "$package" --edges normal --prefix none 2>&1); then
        echo "error: failed to inspect the normal dependency graph for $package"
        printf '%s\n' "$output"
        exit 1
    fi

    if printf '%s\n' "$output" | grep -Eq "^${dependency} v"; then
        echo "error: $package unexpectedly pulls $dependency"
        printf '%s\n' "$output"
        exit 1
    fi
}

echo "── Proving the generated Postcard graph is JSON-free"
assert_not_in_normal_graph postcard-drift-common serde_json

echo "✓ generated default, Postcard, and mixed-codec output passed drift checks"
