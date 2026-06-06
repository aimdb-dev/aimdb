# embassy-serial-connector-demo

A real end-to-end test of [`aimdb-serial-connector`](../../aimdb-serial-connector):
an **STM32H563ZI serves the AimX toolset over a UART**, and a host queries it over
the serial line. This exercises the `no_std` AimX server dispatch (Issue #120) over
the real Embassy serial transport (COBS framing over `embedded-io-async`).

```
┌────────────────────────────┐         USART3 (PD8/PD9)          ┌──────────────────────┐
│ STM32H563ZI (this firmware)│  ⇄  ST-LINK Virtual COM Port  ⇄   │ host: serial_demo    │
│ SerialServer + AimxDispatch│        = /dev/ttyACM0             │ AimX client (RPC)    │
│ records: counter + setting │                                   │ record.list/get loop │
└────────────────────────────┘                                   └──────────────────────┘
```

## Hardware

- **STM32H563ZI Nucleo** (the same board the KNX/MQTT Embassy demos run on).
- The single **USB↔ST-LINK** cable you already use for flashing. On the
  Nucleo-H563ZI the ST-LINK exposes a **Virtual COM Port** bridged to **USART3
  (PD8 = TX, PD9 = RX)**. No extra wiring. The host device node is:
  - **Linux:** `/dev/ttyACM0` (`ls /dev/ttyACM*`)
  - **macOS:** `/dev/cu.usbmodem…` (`ls /dev/cu.usbmodem*` — use the `cu.*`, not
    `tty.*`, node)
- defmt logs stream separately over **RTT (SWD)**, so they don't collide with the
  data UART.

> ⚠️ **The VCP↔USART routing is board-specific** (ST-LINK solder bridges, per
> UM3115). If `record.list` times out, your board likely routes the VCP to a
> different USART — change the `USART3 / PD9 / PD8` line in
> [`src/main.rs`](src/main.rs), or wire a USB-TTL dongle to any free USART and
> point the host at the resulting `/dev/ttyUSB0`.

## Run it

**1. Build the firmware** (in the dev container):

```bash
cd examples/embassy-serial-connector-demo
cargo build            # → ../../target/thumbv8m.main-none-eabihf/debug/embassy-serial-connector-demo
```

**2. Flash + view logs** (on the host, where probe-rs + the ST-LINK live):

```bash
./flash.sh             # = probe-rs run --chip STM32H563ZITx <binary>
# RTT shows: "serving AimX over USART3 / ST-LINK VCP (records: counter, setting) — connect the host now"
```

**3. Query it from the host** over the VCP (run from the **workspace root** — the
demo dir pins a different toolchain/target). Replace the device with yours
(`/dev/ttyACM0` on Linux, `/dev/cu.usbmodem…` on macOS):

```bash
cd ../..   # workspace root
cargo run -p aimdb-serial-connector --example serial_demo \
    --features _test-tokio -- client /dev/ttyACM0 115200
```

Expected output — the `counter` values are produced **on the MCU** and read back
over serial:

```
[client] record.list = [{...,"record_key":"counter","writable":false},{...,"record_key":"setting","writable":true}]
[client] counter = {"value":173}
[client] counter = {"value":174}
[client] counter = {"value":175}
...
```

**4. Write a record** over the line (`record.set`). The board exposes a writable
`setting` record (no producer, ReadWrite policy); this sets it and reads it back:

```bash
cargo run -p aimdb-serial-connector --example serial_demo \
    --features _test-tokio -- set /dev/cu.usbmodem14303 115200
```

```
[set] record.set setting={"level":1} -> {"value":{"level":1}}
[set] record.get setting    -> {"level":1}
[set] record.set setting={"level":2} -> {"value":{"level":2}}
...
```

## No board? Smoke it over a PTY pair

The host example is dual-mode, so two host processes can talk over a virtual serial
pair (needs `socat`):

```bash
socat -d -d pty,raw,echo=0 pty,raw,echo=0      # prints two /dev/pts/N device names
# terminal A — host acts as the board:
cargo run -p aimdb-serial-connector --example serial_demo --features _test-tokio -- server /dev/pts/3
# terminal B — host client:
cargo run -p aimdb-serial-connector --example serial_demo --features _test-tokio -- client /dev/pts/4
```

## Troubleshooting

These bit us during bring-up — check them first:

- **Replug after flashing.** After `probe-rs` resets the MCU, the ST-LINK VCP often
  doesn't re-enumerate cleanly, so the `/dev/cu.usbmodem…` (or `/dev/ttyACM…`)
  handle is stale and the client gets nothing. **Physically unplug/replug the
  board** before running the host (the device suffix may change — re-check with
  `ls`).
- **One program per port.** Only one process may hold the serial device at a time.
  A stray `screen` is the usual culprit — *closing its window only detaches it*, so
  it keeps the port open and silently eats the board's replies. Find and kill
  leftovers: `lsof <device>` then `kill -9 <pid>`; quit `screen` with `Ctrl-A` `K`
  (not by closing the window).
- **Wrong device** — the VCP may enumerate with a different suffix. Linux:
  `ls /dev/ttyACM*` (or `dmesg | tail`). macOS: `ls /dev/cu.usbmodem*`, and use the
  `cu.*` node (the `tty.*` node blocks on open waiting for carrier-detect).
- **Permission denied (Linux)** — add yourself to the `dialout` group
  (`sudo usermod -aG dialout $USER`, then re-login) or `sudo chmod a+rw /dev/ttyACM0`.
- **`record.list` hangs / wrong data** — usually the VCP↔USART mapping (see the
  warning above) or a baud mismatch (the firmware uses **115200**; pass the same to
  the host).
