# Hello SingleLatest Async

This example demonstrates the `SingleLatest` semantics in AimDB using an async runtime.

## What is SingleLatest?

`SingleLatest` is a storage semantic where only the most recent value is retained. When a new value is written, it overwrites the previous one. This is useful for sensor readings, status indicators, or any data point where only the current state matters.

Key characteristics:

- Only the latest value is stored
- Writes always succeed and replace the previous value
- Reads always return the most recently written value
- Minimal memory footprint (single slot per field)

## Running the Example

```sh
cargo run -p hello-single-latest-async
```

## What It Does

The example creates a `SingleLatest` field, writes values to it asynchronously, and reads back the current value to demonstrate that only the most recent write is visible.
