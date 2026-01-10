#!/bin/bash
# Generate TypeScript bindings from Rust contracts and copy to aimdb-ui
#
# Usage: ./scripts/gen-ts-bindings.sh
#
# This script:
# 1. Runs the ts-rs export test to generate TypeScript definitions
# 2. Generates schema-registry.ts with Observable metadata (icons, units)
# 3. Copies the generated files to aimdb-ui/src/types/generated/

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
UI_TYPES_DIR="${AIMDB_UI_PATH:-/aimdb_ws/aimdb-pro/_external/aimdb-ui/src/types/generated}"

echo "🔧 Generating TypeScript bindings from Rust contracts..."

cd "$REPO_ROOT"

# Run the export test (requires both ts and observable features)
cargo test -p aimdb-data-contracts --features ts,observable export_typescript -- --ignored --nocapture

# Check if bindings were generated
BINDINGS_DIR="$REPO_ROOT/aimdb-data-contracts/bindings"
if [ ! -d "$BINDINGS_DIR" ]; then
    echo "❌ Error: bindings directory not found at $BINDINGS_DIR"
    exit 1
fi

echo "📁 Generated bindings:"
ls -la "$BINDINGS_DIR"

# Create target directory if specified and exists
if [ -n "$UI_TYPES_DIR" ]; then
    mkdir -p "$UI_TYPES_DIR"
    
    # Copy bindings (types + schema registry)
    cp "$BINDINGS_DIR"/*.ts "$UI_TYPES_DIR/"
    
    # Create index.ts barrel export
    cat > "$UI_TYPES_DIR/index.ts" << 'EOF'
// Auto-generated from aimdb-data-contracts via ts-rs
// Do not edit manually - run scripts/gen-ts-bindings.sh to regenerate

export type { Temperature } from './Temperature';
export type { Humidity } from './Humidity';
export type { GpsLocation } from './GpsLocation';
export { SCHEMA_REGISTRY, SCHEMA_TYPES, getSchema } from './schema-registry';
export type { SchemaMeta } from './schema-registry';
EOF

    echo "✅ Copied bindings to $UI_TYPES_DIR"
    ls -la "$UI_TYPES_DIR"
else
    echo "ℹ️  Set AIMDB_UI_PATH to copy bindings to UI project"
    echo "   Example: AIMDB_UI_PATH=/path/to/aimdb-ui/src/types/generated ./scripts/gen-ts-bindings.sh"
fi

echo ""
echo "✅ TypeScript bindings generation complete!"
