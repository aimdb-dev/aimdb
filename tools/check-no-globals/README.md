# Production global-state guard

`make check-no-globals` runs this tool's fixtures, then checks the library
roots listed in `GLOBALS_SCANNED` in the Makefile. Applications and test
executables own their processes and may install loggers; libraries must leave
that decision to their hosts. The fork detector in `aimdb-sync/src/fork.rs`
remains the explicit exception because it owns the runtime thread it protects.

The tool uses `syn` to parse Rust and follows inline and external modules,
including explicit `#[path]` declarations. It evaluates explicit `#[cfg(...)]`
attributes with `test = false` and all other predicates unknown. Thus
`#[cfg(all(test, feature = "log"))]` is excluded, while
`#[cfg(any(test, feature = "log"))]` is still checked. Test-only parent modules
exclude their children. Unit tests can stay beside private implementation code.
Missing module files and invalid Rust fail the check. Comments and string
literals do not count as installations.

This is a conservative source guard, not Rust name resolution or macro expansion.
It checks forbidden API names in paths, imports, method calls, and macro tokens;
macro bodies and `cfg_attr` are not evaluated for test exclusion. `include!` is
rejected until its source traversal is supported. Generated or externally
expanded code still needs review. Use explicit test-only modules for test logger
setup rather than adding file exceptions to this guard.

The tool reuses the workspace's existing `syn`, `quote`, and `proc-macro2`
dependencies and is not published or linked into production libraries.
