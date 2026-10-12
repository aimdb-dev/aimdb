# Changelog - aimdb-cdr

All notable changes to the `aimdb-cdr` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **First release: serde for OMG CDR (XCDR1)**, the ROS 2 wire format, with
  its 4-byte encapsulation header. `no_std`, `alloc` optional.
  - `to_slice` encodes little-endian into caller storage without allocating;
    `to_vec` needs `alloc`.
  - `from_bytes` decodes both byte orders, taken from the header (`CDR_LE` or
    `CDR_BE`); any other representation is an error.
  - Alignment is measured from the end of the header; strings carry a `u32`
    length including the NUL, sequences a `u32` count, fixed arrays none.
  - Tested against golden bytes, byte for byte against `hiroz-cdr` for
    little-endian output, and with random and malformed input.
