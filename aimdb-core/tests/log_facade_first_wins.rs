//! Design 050, criterion 6: a second `set_logger` returns `Err` and the first
//! destination keeps receiving.
//!
//! This is the half the C++ door got wrong — a last-wins header over a
//! first-wins C layer. Deciding it once, in `log`, makes that unreproducible.
#![cfg(feature = "log")]

mod log_support;

use log_support::Capture;

static FIRST: Capture = Capture::new();
static SECOND: Capture = Capture::new();

#[test]
fn the_first_destination_wins_and_the_second_is_told() {
    log::set_logger(&FIRST).expect("the first install succeeds");
    log::set_max_level(log::LevelFilter::Trace);

    aimdb_core::log_warn!("before the second install");
    assert_eq!(FIRST.count(), 1);

    assert!(
        log::set_logger(&SECOND).is_err(),
        "a second set_logger must fail"
    );

    aimdb_core::log_warn!("after the second install");

    assert_eq!(
        FIRST.count(),
        2,
        "the first destination stopped receiving after a refused second install"
    );
    assert_eq!(
        SECOND.count(),
        0,
        "a refused destination received events anyway"
    );
}
