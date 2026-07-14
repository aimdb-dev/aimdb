# hello-spmc-ring-async: async Spmc Ring buffer demo
The Spmc Ring buffer supports Single Producer - Multi Consumer pattern. It features a ring buffer with fixed capacity. Each consumer could read the full sequence at its own rate without waiting for consumption states of other consumers.

## When to use
This Spmc Ring buffer demo is useful in a variety of scenarios where you need to broadcast messages to different types of consumers but you are not sure if one could lag behind others.

## How it works
The Spmc buffer demo works by simulating:
- A producer generating a message at fixed interval (struct `Temperature`);
- A consumer/observer 01 consuming at a rate faster than producer's;
- A consumer/observer 02 consuming at a rate slower than producer's, and eventually lagging behind (messages dropped), as the ring size is small (10 slots);

## How to run
From the workspace root, run:
```
cargo run -p hello-spmc-ring-async
```
**Expected output**
```
=== hello-spmc-ring-async: SPMC ring buffer demo ===

Ring size:          10
Producer at rate    20.0 messages/sec
Observer 01 at rate 25.0 message/sec
Observer 02 at rate  6.7 message/sec


Observer 01: At 1783932208: -18 celcius
Observer 02: At 1783932208: -18 celcius
Observer 01: At 1783932208: -16 celcius
Observer 01: At 1783932208: -14 celcius
...
Observer 02: At 1783932208: -2 celcius
Observer 01: At 1783932209: 30 celcius
Observer 01: At 1783932209: 32 celcius
Observer 01: At 1783932209: 34 celcius
Observer 02 lagged, dropped 2
Observer 02: At 1783932209: 4 celcius
...

```
