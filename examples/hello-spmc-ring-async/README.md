# hello-spmc-ring-async

This example demonstrates the `SpmcRing` primitive in AimDB — a bounded ring buffer where each consumer maintains its own read cursor. Slow consumers can lag independently without blocking the producer; when the ring is full, the oldest entries are overwritten. This makes `SpmcRing` ideal for sensor telemetry and event logs where you want bounded memory usage and multiple independent readers.

## When to use SpmcRing

- **SpmcRing** — bounded history, multiple independent consumers, oldest-overwrite semantics.
- **SingleLatest** — only the latest value is retained, no history.
- **Mailbox** — single consumer, point-to-point delivery.

Pick `SpmcRing` when you need more than one reader and care about recent history rather than just the current value.

## Running

```sh
cargo run -p hello-spmc-ring-async
```
