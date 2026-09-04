//! The MCU's channel and select types must work in a **linked** std binary, so
//! one connection task can serve both runtimes.
//!
//! `CriticalSectionRawMutex` is the only `Sync` raw mutex `embassy-sync` offers
//! — `NoopRawMutex` is `!Sync` and cannot back a shared channel at all — and
//! using it pulls in `_critical_section_1_0_acquire`/`_release`, which nothing
//! defines on std. Selecting an impl is the final binary's call, so the library
//! does not make it: a test binary is a binary, and gets the std impl through
//! this crate's `critical-section` dev-dependency. These tests fail to *link*,
//! not to compile, if that ever comes undone.
#![cfg(feature = "tokio-runtime")]

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;

type Cmd = (u8, u16);

/// A send and receive through the channel, exercising the critical section.
#[tokio::test]
async fn embassy_channel_carries_a_value_on_std() {
    let channel: Channel<CriticalSectionRawMutex, Cmd, 4> = Channel::new();

    channel.send((1, 0x0a0b)).await;
    assert_eq!(channel.receive().await, (1, 0x0a0b));
}

/// The channel is `Send + Sync`, so a spawned task can enqueue while the
/// protocol loop drains — what the unified connection task relies on.
#[tokio::test]
async fn embassy_channel_is_shareable_across_tasks_on_std() {
    static CHANNEL: Channel<CriticalSectionRawMutex, Cmd, 4> = Channel::new();

    let producer = tokio::spawn(async {
        CHANNEL.send((2, 0x0c0d)).await;
    });

    assert_eq!(CHANNEL.receive().await, (2, 0x0c0d));
    producer.await.expect("producer task");
}

/// `embassy-futures` drives the same select on std as on the MCU.
#[tokio::test]
async fn embassy_select_resolves_on_std() {
    use embassy_futures::select::{select, Either};

    let channel: Channel<CriticalSectionRawMutex, Cmd, 4> = Channel::new();
    channel.send((3, 0x0e0f)).await;

    match select(channel.receive(), core::future::pending::<()>()).await {
        Either::First(cmd) => assert_eq!(cmd, (3, 0x0e0f)),
        Either::Second(()) => panic!("pending future must never win"),
    }
}
