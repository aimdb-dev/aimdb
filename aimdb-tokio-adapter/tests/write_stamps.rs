//! Design 058: every write is stamped from the instance clock, and every read
//! returns the stamp of the write it reads.
//!
//! Runs on a `ManualClock`, so a stamp is exactly the time the test set.

use std::sync::Arc;
use std::time::Duration;

use aimdb_core::buffer::BufferCfg;
use aimdb_core::executor::test_support::ManualClockRuntimeOps;
use aimdb_core::{AimDb, AimDbBuilder, Stamp};
use aimdb_tokio_adapter::TokioRecordRegistrarExt;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Input(u32);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Output(u32);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Other(u32);

const INPUT: &str = "stamps::Input";
const OUTPUT: &str = "stamps::Output";
const JOINED: &str = "stamps::Joined";
const OTHER: &str = "stamps::Other";
const CONFIG: &str = "stamps::Config";

fn ms(n: u64) -> Stamp {
    Stamp::from_nanos(n * 1_000_000)
}

/// Sets the manual clock to `t` (it only moves forward).
fn set(ops: &ManualClockRuntimeOps, t: Stamp) {
    let now = aimdb_core::InstanceClock::now(&*ops.clock);
    ops.clock.advance(t.since(now));
}

async fn recv<T: Clone + Send + 'static>(
    reader: &mut aimdb_core::buffer::Reader<T>,
) -> aimdb_core::Stamped<T> {
    tokio::time::timeout(Duration::from_secs(2), reader.recv())
        .await
        .expect("a value within 2 s")
        .expect("reader open")
}

/// Input → Output (single transform), Input + Other → Joined (join, which
/// reports each trigger's input stamp), and a remotely writable Config.
async fn db(
    ops: &ManualClockRuntimeOps,
) -> (AimDb, tokio::sync::mpsc::UnboundedReceiver<(u32, Stamp)>) {
    let (trigger_tx, trigger_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut builder = AimDbBuilder::new().runtime(Arc::new(ops.clone()));
    builder.configure::<Input>(INPUT, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
            .with_remote_access();
    });
    builder.configure::<Other>(OTHER, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 });
    });
    builder.configure::<Output>(OUTPUT, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
            .transform::<Input, _>(INPUT, |b| b.map(|i: &Input| Some(Output(i.0 * 10))));
    });
    builder.configure::<Output>(JOINED, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 })
            .transform_join(|b| {
                b.input::<Input>(INPUT).input::<Other>(OTHER).on_triggers(
                    move |mut rx, producer| async move {
                        while let Ok(trigger) = rx.recv().await {
                            if let Some(input) = trigger.as_input::<Input>() {
                                trigger_tx.send((input.0, trigger.stamp())).unwrap();
                                producer.produce(Output(input.0));
                            }
                        }
                    },
                )
            });
    });
    builder.configure::<Input>(CONFIG, |reg| {
        reg.buffer(BufferCfg::SingleLatest).with_remote_access();
    });
    let (db, runner) = builder.build().await.unwrap();
    tokio::spawn(runner.run());
    // Let transforms and join forwarders subscribe.
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    (db, trigger_rx)
}

/// Source writes (`AimDb::produce`, a `Producer`, `try_produce`) are stamped
/// with the clock reading at the write; `recv`, `latest` and the JSON reads
/// all return that stamp.
#[tokio::test(flavor = "current_thread")]
async fn source_writes_are_stamped_and_every_read_returns_the_stamp() {
    let ops = ManualClockRuntimeOps::default();
    let (db, _triggers) = db(&ops).await;
    let mut reader = db.subscribe::<Input>(INPUT).unwrap();
    let producer = db.producer::<Input>(INPUT).unwrap();

    set(&ops, ms(5));
    db.produce(INPUT, Input(1)).unwrap();
    set(&ops, ms(7));
    producer.produce(Input(2));
    set(&ops, ms(9));
    producer.try_produce(Input(3)).unwrap();

    for (value, stamp) in [(1, ms(5)), (2, ms(7)), (3, ms(9))] {
        let sample = recv(&mut reader).await;
        assert_eq!((&sample.value, sample.stamp()), (&Input(value), stamp));
    }

    // SingleLatest: latest, its JSON forms and a subscriber agree.
    set(&ops, ms(11));
    db.produce(CONFIG, Input(4)).unwrap();
    set(&ops, ms(12));
    let latest = db.latest::<Input>(CONFIG).unwrap().unwrap();
    assert_eq!((&latest.value, latest.stamp()), (&Input(4), ms(11)));
    let json = db.try_latest_as_json(CONFIG).unwrap();
    assert_eq!((&json.value, json.stamp()), (&serde_json::json!(4), ms(11)));
    let bytes = db.try_latest_as_json_bytes(CONFIG).unwrap();
    assert_eq!((&bytes.value, bytes.stamp()), (&b"4".to_vec(), ms(11)));
    let mut config_reader = db.subscribe::<Input>(CONFIG).unwrap();
    assert_eq!(recv(&mut config_reader).await.stamp(), ms(11));
}

/// A transform's and a join's output is a new write, stamped when it is
/// written, not with its input's stamp. The join still sees the input's.
#[tokio::test(flavor = "current_thread")]
async fn derived_outputs_carry_their_own_write_time() {
    let ops = ManualClockRuntimeOps::default();
    let (db, mut triggers) = db(&ops).await;
    let mut output = db.subscribe::<Output>(OUTPUT).unwrap();
    let mut joined = db.subscribe::<Output>(JOINED).unwrap();

    set(&ops, ms(5));
    db.produce(INPUT, Input(2)).unwrap();
    // No await since the write: the transform and the join run after the
    // clock moved on, so their outputs are stamped later than the input.
    set(&ops, ms(8));

    let out = recv(&mut output).await;
    assert_eq!((&out.value, out.stamp()), (&Output(20), ms(8)));

    let (input, input_stamp) = tokio::time::timeout(Duration::from_secs(2), triggers.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (input, input_stamp),
        (2, ms(5)),
        "the trigger carries the input's write"
    );
    let joined = recv(&mut joined).await;
    assert_eq!((&joined.value, joined.stamp()), (&Output(2), ms(8)));
}

/// A remote `record.set` is a write like any other.
#[tokio::test(flavor = "current_thread")]
async fn a_remote_set_is_stamped_when_applied() {
    let ops = ManualClockRuntimeOps::default();
    let (db, _triggers) = db(&ops).await;

    set(&ops, ms(3));
    db.set_record_from_json(CONFIG, serde_json::json!(9))
        .unwrap();
    let latest = db.latest::<Input>(CONFIG).unwrap().unwrap();
    assert_eq!((&latest.value, latest.stamp()), (&Input(9), ms(3)));
}

/// Design 058 R5: under Tokio paused time, write stamps follow `tokio::time`
/// exactly, so simulations are reproducible.
#[tokio::test(start_paused = true)]
async fn under_paused_time_stamps_follow_tokio_time_exactly() {
    let mut builder =
        AimDbBuilder::new().runtime(Arc::new(aimdb_tokio_adapter::TokioAdapter::new().unwrap()));
    builder.configure::<Input>(INPUT, |reg| {
        reg.buffer(BufferCfg::SpmcRing { capacity: 16 });
    });
    let (db, runner) = builder.build().await.unwrap();
    tokio::spawn(runner.run());
    let mut reader = db.subscribe::<Input>(INPUT).unwrap();

    db.produce(INPUT, Input(1)).unwrap();
    db.produce(INPUT, Input(2)).unwrap();
    let d = Duration::from_millis(1234);
    tokio::time::advance(d).await;
    db.produce(INPUT, Input(3)).unwrap();

    let first = reader.recv().await.unwrap().stamp();
    let second = reader.recv().await.unwrap().stamp();
    let third = reader.recv().await.unwrap().stamp();
    assert_eq!(first, second, "no time passed between the two writes");
    assert_eq!(third.since(second), d);
    assert_eq!(db.now(), third, "nothing ran after the third write");
}
