# Changelog - aimdb-build-info

All notable changes to the `aimdb-build-info` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **First release: the build-info section, format 1**. `no_std`,
  no dependencies.
  - `parse` reads a section from a binary or a flash dump that may extend past
    it, checks every header rule, and returns an error rather than panicking
    on malformed input. The payload starts at `header_len`, so later headers
    can grow.
  - `Section::post_link` reports the post-link region as absent, erased,
    written or malformed.
