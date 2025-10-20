//! Integration tests for subscription and dispatch functionality
//!
//! Tests the end-to-end flow: producer → buffer → consumers with subscription support

use aimdb_core::buffer::{BufferBackend, BufferCfg};
use aimdb_core::{Database, DbResult, RecordRegistrar, RecordT};
use aimdb_tokio_adapter::{TokioBuffer, TokioDatabaseBuilder};

#[derive(Debug, Clone, PartialEq)]
struct TestData {
    value: i32,
}

struct TestConfig;

impl RecordT for TestData {
    type Config = TestConfig;

    fn register<'a>(reg: &'a mut RecordRegistrar<'a, Self>, _cfg: &Self::Config) {
        // Create buffer for async dispatch
        let buffer = TokioBuffer::<TestData>::new(&BufferCfg::SpmcRing { capacity: 100 });

        reg.buffer(Box::new(buffer))
            .producer(|_emitter, data| async move {
                #[cfg(feature = "tracing")]
                tracing::info!("Producer received: {:?}", data);
            })
            .consumer(|_emitter, data| async move {
                #[cfg(feature = "tracing")]
                tracing::info!("Consumer received: {:?}", data);
            });
    }
}

#[tokio::test]
async fn test_subscribe_to_buffered_record() -> DbResult<()> {
    // Build database with buffered record
    let db = Database::builder()
        .record::<TestData>(&TestConfig)
        .build()?;

    // Subscribe to the buffer
    let mut reader = db.subscribe::<TestData>()?;

    // Get emitter for producing data
    let emitter = db.emitter();

    // Spawn a task to produce data
    tokio::spawn(async move {
        for i in 0..5 {
            let _ = emitter.emit(TestData { value: i }).await;
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }
    });

    // Collect values from the reader
    let mut received = Vec::new();
    for _ in 0..5 {
        if let Ok(Ok(value)) =
            tokio::time::timeout(tokio::time::Duration::from_secs(1), reader.recv()).await
        {
            received.push(value);
        }
    }

    // Verify we received the data
    assert_eq!(received.len(), 5);
    for (i, data) in received.iter().enumerate() {
        assert_eq!(data.value, i as i32);
    }

    Ok(())
}

#[tokio::test]
async fn test_subscribe_to_nonexistent_record() {
    #[derive(Debug, Clone)]
    struct UnregisteredData {
        _value: i32,
    }

    let db = Database::builder()
        .record::<TestData>(&TestConfig)
        .build()
        .unwrap();

    // Attempt to subscribe to unregistered type should fail
    let result = db.subscribe::<UnregisteredData>();
    assert!(result.is_err());
}

#[tokio::test]
async fn test_subscribe_to_unbuffered_record() {
    #[derive(Debug, Clone)]
    struct UnbufferedData {
        _value: i32,
    }

    struct UnbufferedConfig;

    impl RecordT for UnbufferedData {
        type Config = UnbufferedConfig;

        fn register<'a>(reg: &'a mut RecordRegistrar<'a, Self>, _cfg: &Self::Config) {
            // No buffer configured
            reg.producer(|_emitter, _data| async move {})
                .consumer(|_emitter, _data| async move {});
        }
    }

    let db = Database::builder()
        .record::<UnbufferedData>(&UnbufferedConfig)
        .build()
        .unwrap();

    // Attempt to subscribe to unbuffered record should fail
    let result = db.subscribe::<UnbufferedData>();
    assert!(result.is_err());
}
