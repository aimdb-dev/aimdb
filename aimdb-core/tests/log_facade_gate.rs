//! Design 050, acceptance criterion 2: below the installed level, an event
//! costs the two level loads and formats nothing.
//!
//! Restricted to the `log`-only build. With `tracing` also on, its arm runs too
//! and the formatting count would be measuring both arms at once — a
//! `tracing`-shaped question, and not the one this criterion asks.
#![cfg(all(feature = "log", not(feature = "tracing")))]

mod log_support;

use log_support::{Capture, FormatProbe};

static CAPTURE: Capture = Capture::new();
static PROBE: FormatProbe = FormatProbe::new();

#[test]
fn a_filtered_event_never_reaches_its_arguments() {
    log::set_logger(&CAPTURE).expect("no other logger may be installed in this binary");

    // Below the gate: `log`'s macro checks STATIC_MAX_LEVEL and max_level()
    // before it builds the `Record`, so `Display` is never called.
    log::set_max_level(log::LevelFilter::Warn);
    aimdb_core::log_info!("gate probe: {}", PROBE);

    assert_eq!(
        PROBE.formatted(),
        0,
        "a filtered-out event formatted its arguments"
    );
    assert_eq!(CAPTURE.count(), 0, "a filtered-out event was delivered");

    // Above the gate: the same call site now formats exactly once. Without this
    // half, a facade that had quietly stopped emitting altogether would pass.
    log::set_max_level(log::LevelFilter::Trace);
    aimdb_core::log_info!("gate probe: {}", PROBE);

    assert_eq!(
        PROBE.formatted(),
        1,
        "an admitted event did not format its arguments exactly once"
    );
    let delivered = CAPTURE.taken();
    assert_eq!(
        delivered.len(),
        1,
        "expected one delivery, got {delivered:?}"
    );
    assert_eq!(delivered[0].message, "gate probe: probe");
}
