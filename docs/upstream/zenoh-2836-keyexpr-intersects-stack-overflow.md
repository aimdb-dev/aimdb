# zenoh#2836 — `intersects` overflows the stack on long keys

- **Upstream:** [eclipse-zenoh/zenoh#2836](https://github.com/eclipse-zenoh/zenoh/issues/2836)
- **Filed:** 2026-10-08, by us
- **Affects:** zenoh-keyexpr 1.10.1, and `main` @ 74051d0
- **Status:** open

## Why it matters to AimDB

`ZenohGrammar` does its own matching and does not call `intersects`; it
handles a 64 KB key in about a millisecond (`long_keys_match_in_linear_time`).
zenoh calls it, though: `Session` matches every sample it delivers against
each subscriber's key expression with `intersects` (`subscriber_callbacks` in
`zenoh/src/api/session.rs`, 1.10.1). So an inbound `zenoh://` link whose
filter has a `**` that other chunks follow, such as
`zenoh://site/{path..}/state`, is reachable from the network: any client that
can publish a matching key through the router can abort the process.

Measured on zenoh 1.10.1, release build. A client subscribed to `a/**/b`
through a router received one `put` on a matching key of 32,000 chunks
(64,001 bytes, inside the wire format's limit). Its receive thread (`rx-0`)
overflowed its stack and the process aborted; the router survived. Peer to
peer, `**/b` and `a/*/**/b` aborted the same way. A trailing `**` is not
affected: `a/**` received a key of 32,000 chunks, because `intersects`
returns at once when the `**` is last.

The connector has no guard yet; its README lists this under Limits. A guard
could declare such a filter only up to its first `**` and match the filter
itself in the callback, except a filter with a verbatim chunk after that
`**`, which no wider key covers. The issue also matters if the connector ever
delegates matching to `zenoh-keyexpr`, and for the grammar oracle, whose
corpus keeps keys short.

## Issue as filed

### Describe the bug

`keyexpr::intersects` recurses once per chunk when one side has a `**`, so its stack depth grows with the length of the other key expression. A long enough key overflows the stack and aborts the process. A stack overflow is not a panic, so `catch_unwind` cannot recover from it.

The recursion is in `it_intersect` in `commons/zenoh-keyexpr/src/key_expr/intersect/classical.rs` (identical in 1.10.1 and on `main` @ 74051d0):

```rust
(b"**", _) => {
    if advanced1.is_empty() {
        return !it2.has_verbatim();
    }
    return (!current2.has_direct_verbatim_non_empty()
        && it_intersect::<STAR_DSL>(it1, advanced2))
        || it_intersect::<STAR_DSL>(advanced1, it2);
}
```

`it_intersect(it1, advanced2)` advances the other side by one chunk per call, and nothing turns it into a loop, so a key of `n` chunks against `a/**/b` nests about `n` calls deep. Measured on a spawned thread with the default 2 MiB stack, release build:

| Key | `intersects("a/**/b", key)` | `includes("a/**/b", key)` |
|---|---|---|
| `a/a/…/a`, 16,000 chunks (32 KB) | `false` | `false` |
| `a/a/…/a`, 32,000 chunks (64 KB) | **stack overflow, process aborted** | `false` |

`includes` handles the same key, so the depth is specific to `intersects`.

Because each `**` branches into two recursive calls, time is also quadratic in the key for patterns with two `**`. On a thread with a 1 GiB stack, so the overflow doesn't interfere:

| Key chunks | `intersects("**/a/**/b", key)` |
|---|---|
| 4,000 | 0.20 s |
| 8,000 | 0.80 s |
| 16,000 | 3.46 s |

Expected: `intersects` should not overflow the stack for any key that `keyexpr::new` accepts. Cost should also stay near-linear in the key's length, as `includes` already shows for this input.

Why it matters: an application that calls `intersects` on a key expression it received, for example to check a sample's key against its own filters, can be aborted by a single long key, or slowed down for seconds by one sample.

### To reproduce

```toml
[dependencies]
zenoh-keyexpr = "=1.10.1"
```

```rust
use zenoh_keyexpr::keyexpr;

fn main() {
    // Default spawned-thread stack: 2 MiB.
    std::thread::spawn(|| {
        let key = vec!["a"; 32_000].join("/");
        let key = keyexpr::new(key.as_str()).unwrap();
        let filter = keyexpr::new("a/**/b").unwrap();
        println!("includes: {}", filter.includes(key)); // false
        println!("intersects: {}", filter.intersects(key)); // stack overflow
    })
    .join()
    .unwrap();
}
```

`cargo run --release` prints `includes: false`, then `fatal runtime error: stack overflow, aborting`.

### System info

- zenoh-keyexpr 1.10.1 (crates.io); the code is unchanged on `main` @ 74051d0
- rustc 1.98.1, Linux x86_64
- Found while testing a key-expression matcher against `zenoh-keyexpr` as a reference
