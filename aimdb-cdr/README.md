# aimdb-cdr

`no_std` serde encoding for OMG CDR (XCDR1) with the 4-byte encapsulation
header — the payload format ROS 2 puts on the wire.

- Encodes little-endian; decodes little- and big-endian.
- `to_slice` writes into caller storage without allocating. `to_vec` and owned
  `String`/`Vec` fields need the default `alloc` feature.
- Strings carry a `u32` length including the NUL; sequences a `u32` count;
  fixed arrays and structs no prefix. Alignment is measured from the end of the
  header.

```rust
#[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
struct Temperature { temperature: f64, variance: f64 }

let mut buf = [0u8; 20];
let len = aimdb_cdr::to_slice(&Temperature { temperature: 21.5, variance: 0.0 }, &mut buf).unwrap();
let back: Temperature = aimdb_cdr::from_bytes(&buf[..len]).unwrap();
assert_eq!(back.temperature, 21.5);
```

`Option`, enums, maps and `char` have no XCDR1 mapping in ROS 2 and are
rejected with `Error::Unsupported`.
