# hello-spmc-ring: sync Spmc Ring buffer demo
The Spmc Ring buffer supports Single Producer - Multi Consumer pattern. It features a ring buffer with fixed capacity. Each consumer could read the full sequence at its own rate without waiting for consumption states of other consumers provided it doesn't fall behind by more values than the configured capacity. If a reader falls behind by more entries than the configured capacity, the oldest unread entries are dropped and the reader reports that it lagged.

In contrast to [SingleLatest](../hello-single-latest), slow observer doesn't skip overwritten intermediate values and it receives every value because `SpmcRing` keeps one shared bounded sequence and every consumer tracks an independent read position.

## When to use
This Spmc Ring buffer demo is useful in a variety of scenarios where you need to broadcast messages to different types of consumers but you are not sure if one could lag behind others.

## How it works
The sync Spmc buffer demo works by simulating:
- A sync producer generating a message at fixed interval (struct `Temperature`);
- A fast consumer/observer consuming at the same rate of the producer's;
- A slow consumer/observer consuming at a rate slower than producer's;

## How to run
From the workspace root, run:
```
cargo run -p hello-spmc-ring
```
**Expected output**
```
=== hello-spmc-ring: SPMC ring buffer sync demo ===

Ring size:          10
Producer interval:   50 ms
Fast tap delay:      none
Slow tap delay:      100 ms


1. Building database and attaching for sync API...
   Main: Setting Temperature At 1784318569: 30°C
[info] [fast] observed temperature At 1784318569: 30°C
[info] [slow] observed temperature At 1784318569: 30°C
   Main: Setting Temperature At 1784318569: 30.5°C
[info] [fast] observed temperature At 1784318569: 30.5°C
[info] [slow] observed temperature At 1784318569: 30.5°C
   Main: Setting Temperature At 1784318569: 31°C
[info] [fast] observed temperature At 1784318569: 31°C
   Main: Setting Temperature At 1784318570: 31.5°C
[info] [fast] observed temperature At 1784318570: 31.5°C
[info] [slow] observed temperature At 1784318569: 31°C
   Main: Setting Temperature At 1784318570: 32°C
[info] [fast] observed temperature At 1784318570: 32°C
   Main: Setting Temperature At 1784318570: 32.5°C
[info] [fast] observed temperature At 1784318570: 32.5°C
[info] [slow] observed temperature At 1784318570: 31.5°C
   Main: Setting Temperature At 1784318570: 33°C
[info] [fast] observed temperature At 1784318570: 33°C
   Main: Setting Temperature At 1784318570: 33.5°C
[info] [fast] observed temperature At 1784318570: 33.5°C
[info] [slow] observed temperature At 1784318570: 32°C
   Main: Setting Temperature At 1784318570: 34°C
[info] [fast] observed temperature At 1784318570: 34°C
   Main: Setting Temperature At 1784318570: 34.5°C
[info] [fast] observed temperature At 1784318570: 34.5°C
[info] [slow] observed temperature At 1784318570: 32.5°C
[info] [slow] observed temperature At 1784318570: 33°C
[info] [slow] observed temperature At 1784318570: 33.5°C
[info] [slow] observed temperature At 1784318570: 34°C
[info] [slow] observed temperature At 1784318570: 34.5°C

```