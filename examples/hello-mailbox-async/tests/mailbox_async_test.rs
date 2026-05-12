use aimdb_core::prelude::*;
use aimdb_tokio_adapter::TokioAdapter;
use std::time::Duration;
use tokio::time::sleep;

async fn build_db() -> AimDB<TokioAdapter> {
    AimDB::builder()
        .adapter(TokioAdapter::default())
        .build()
        .await
        .expect("failed to build AimDB instance")
}

#[tokio::test]
async fn test_mailbox_latest_wins() {
    let db = build_db().await;

    let mailbox = db
        .mailbox::<&str>("led/command")
        .source(|| async { "initial" })
        .build()
        .await
        .expect("failed to create mailbox");

    mailbox.send("red").await.expect("send red");
    mailbox.send("green").await.expect("send green");
    mailbox.send("blue").await.expect("send blue");

    sleep(Duration::from_millis(10)).await;

    let latest = mailbox.tap().await.expect("tap failed");
    assert_eq!(latest, "blue", "mailbox should retain only the latest value");
}

#[tokio::test]
async fn test_mailbox_single_send() {
    let db = build_db().await;

    let mailbox = db
        .mailbox::<u32>("sensor/temp")
        .source(|| async { 0 })
        .build()
        .await
        .expect("failed to create mailbox");

    mailbox.send(42).await.expect("send 42");

    sleep(Duration::from_millis(10)).await;

    let latest = mailbox.tap().await.expect("tap failed");
    assert_eq!(latest, 42);
}

#[tokio::test]
async fn test_mailbox_tap_returns_source_default_when_no_send() {
    let db = build_db().await;

    let mailbox = db
        .mailbox::<&str>("led/idle")
        .source(|| async { "off" })
        .build()
        .await
        .expect("failed to create mailbox");

    let latest = mailbox.tap().await.expect("tap failed");
    assert_eq!(latest, "off", "tap should return source default when nothing was sent");
}
