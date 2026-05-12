# Hello Mailbox (Async)

This example demonstrates the async mailbox API in AimDB. It shows how to send and receive messages between tasks using non-blocking, future-based operations.

## How it differs from the sync sibling

The `hello-mailbox` example uses the synchronous API, where send and receive calls block the current thread until the operation completes. This async variant instead returns futures that can be `.await`ed, allowing the runtime to schedule other work while waiting. This makes it suitable for use with async executors like Tokio or Embassy.

## Running

```sh
cargo run -p hello-mailbox-async
```
