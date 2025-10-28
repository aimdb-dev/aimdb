#!/bin/bash
# Flash script for embassy-mqtt-connector-demo
# 
# This script should be run on the HOST machine where probe-rs and hardware are accessible.
# The binary must be built first in the dev container using: cargo build

set -e

BINARY="../../target/thumbv8m.main-none-eabihf/debug/embassy-mqtt-connector-demo"

if [ ! -f "$BINARY" ]; then
    echo "Error: Binary not found at $BINARY"
    echo "Please build it first in the dev container:"
    echo "  cd examples/embassy-mqtt-connector-demo && cargo build"
    exit 1
fi

echo "Flashing embassy-mqtt-connector-demo to STM32H563ZITx..."
probe-rs run --chip STM32H563ZITx "$BINARY"
