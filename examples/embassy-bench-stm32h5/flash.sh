#!/bin/bash
# Flash script for embassy-bench-stm32h5 (AimDB B3 on-target profiling).
#
# Run on the HOST machine where probe-rs and the ST-LINK are accessible.
# Build first (in the dev container):
#   cd examples/embassy-bench-stm32h5 && cargo build --release
#
# B3 cycle counts are only meaningful in --release: debug vs release differ by an
# order of magnitude (design 038 §15.8). This script therefore prefers the
# release binary and only falls back to debug with a warning.
#
# Results (cycles/msg + allocs/msg per profile) stream over RTT (SWD) as defmt
# logs — probe-rs prints them to this terminal.
set -e

RELEASE_BINARY="../../target/thumbv8m.main-none-eabihf/release/embassy-bench-stm32h5"
DEBUG_BINARY="../../target/thumbv8m.main-none-eabihf/debug/embassy-bench-stm32h5"

if [ -f "$RELEASE_BINARY" ]; then
    BINARY="$RELEASE_BINARY"
elif [ -f "$DEBUG_BINARY" ]; then
    BINARY="$DEBUG_BINARY"
    echo "Warning: using the DEBUG build — B3 cycle counts are not representative."
    echo "         Rebuild with --release for meaningful numbers:"
    echo "           cd examples/embassy-bench-stm32h5 && cargo build --release"
else
    echo "Error: no binary found at:"
    echo "  $RELEASE_BINARY"
    echo "  $DEBUG_BINARY"
    echo "Build it first in the dev container:"
    echo "  cd examples/embassy-bench-stm32h5 && cargo build --release"
    exit 1
fi

echo "Flashing embassy-bench-stm32h5 to STM32H563ZITx (B3 results stream over RTT)..."
probe-rs run --chip STM32H563ZITx "$BINARY"
