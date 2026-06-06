#!/bin/bash
# Flash script for embassy-serial-connector-demo.
# Run on the HOST where probe-rs + the ST-LINK are accessible.
# Build first (in the dev container): cd examples/embassy-serial-connector-demo && cargo build
set -e
BINARY="../../target/thumbv8m.main-none-eabihf/debug/embassy-serial-connector-demo"
if [ ! -f "$BINARY" ]; then
    echo "Error: Binary not found at $BINARY"
    echo "Build it first: cd examples/embassy-serial-connector-demo && cargo build"
    exit 1
fi
echo "Flashing embassy-serial-connector-demo to STM32H563ZITx (defmt logs stream over RTT)..."
probe-rs run --chip STM32H563ZITx "$BINARY"
