//! A real producer -> MQTT -> consumer rolling upgrade, with separately
//! compiled, locked contracts revisions. Included by backend_parity so its
//! host clock and defmt shim are shared.
use super::*;
use aimdb_core::envelope::{ContractRef, Envelope, HeaderWriter, PayloadCodec};
use aimdb_core::{InboundDispatch, InboundRouteStats};
use aimdb_data_contracts::{
    link_codecs::{Cdr, Json, Postcard},
    name_id, ContractShape, EnvelopeCarrier, EnvelopeLinkExt, LinkCodec, LinkCodecBuilderExt,
    MigrationChain, PayloadEncoder,
};
use rolling_contracts_n as firmware;
use rolling_contracts_n1 as hub;

fn stats(db: &aimdb_core::AimDb, index: usize) -> InboundRouteStats {
    InboundDispatch::new(db, "mqtt", &aimdb_mqtt_connector::MqttGrammar)
        .unwrap()
        .stats(index)
        .unwrap()
}

async fn subscribed(seen: &Mutex<Seen>, count: usize) {
    loop {
        if seen.lock().unwrap().subscribed_topics().len() >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn accepted(db: &aimdb_core::AimDb, index: usize) -> InboundRouteStats {
    loop {
        let snapshot = stats(db, index);
        if snapshot.accepted == 1 {
            return snapshot;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn rolling<C>(codec: C)
where
    C: Copy + 'static + LinkCodec<firmware::Temperature> + LinkCodec<hub::Temperature>,
{
    assert_eq!(firmware::Temperature::SHAPE, hub::Temperature::shape_of(1));
    assert!(firmware::Temperature::SHAPE.is_some());
    assert!(hub::Temperature::SHAPE.is_some());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mut mcu = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(
            MqttConnector::new(url.clone())
                .transport(TokioNet::tcp())
                .with_client_id("rolling-mcu"),
        );
    mcu.configure(firmware::INDOOR_TEMP, |r| {
        r.buffer(BufferCfg::SingleLatest);
        firmware::publish(r, codec);
    });
    mcu.configure(firmware::RETURN_TEMP, |r| {
        r.buffer(BufferCfg::SingleLatest);
        firmware::receive(r, codec);
    });
    let (mcu, mcu_runner) = mcu.build().await.unwrap();
    let mut b = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(MqttConnector::new(url.clone()).with_client_id("rolling-hub"));
    b.configure(hub::INDOOR_TEMP, |r| {
        r.buffer(BufferCfg::SingleLatest);
        hub::receive(r, codec);
    });
    b.configure(hub::OUTDOOR_TEMP, |r| {
        r.buffer(BufferCfg::SingleLatest)
            .link_from(hub::OUTDOOR_URL)
            .with_link_codec(codec)
            .with_envelope(EnvelopeCarrier::Inline)
            .finish();
    });
    b.configure(hub::RETURN_TEMP, |r| {
        r.buffer(BufferCfg::SingleLatest);
        hub::send_to_firmware(r, codec);
    });
    let (db, runner) = b.build().await.unwrap();
    let mut second = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(MqttConnector::new(url).with_client_id("rolling-second-sensor"));
    second.configure(hub::OUTDOOR_TEMP, |r| {
        r.buffer(BufferCfg::SingleLatest)
            .link_to(hub::OUTDOOR_URL)
            .with_link_codec(codec)
            .with_envelope(EnvelopeCarrier::Inline)
            .finish();
    });
    let (second, second_runner) = second.build().await.unwrap();
    let mut indoor = db.consumer(hub::INDOOR_TEMP).unwrap().subscribe();
    let mut outdoor = db.consumer(hub::OUTDOOR_TEMP).unwrap().subscribe();
    let mut returned = mcu.consumer(firmware::RETURN_TEMP).unwrap().subscribe();
    tokio::select! {
        _ = mcu_runner.run() => panic!("MCU stopped"),
        _ = runner.run() => panic!("hub stopped"),
        _ = second_runner.run() => panic!("second sensor stopped"),
        _ = common::forwarding_broker(listener, seen.clone()) => panic!("broker stopped"),
        _ = async {
            subscribed(&seen, 3).await;
            mcu.producer(firmware::INDOOR_TEMP).unwrap()
                .produce(firmware::Temperature { temp: 21.5 });
            let value = indoor.recv().await.unwrap();
            assert_eq!(value.value, hub::Temperature { celsius: 21.5, quality: 1 });
            assert!(value.stamp().as_nanos() > 0, "ingest must be stamped by hub");
            // Firmware was not asked to invent the hub's new quality field.
            accepted(&db, 0).await;
            assert_eq!(stats(&db, 0).accepted, 1);
            assert_eq!(stats(&db, 0).upgraded[0].count, 1);
            assert_eq!(stats(&db, 0).upgraded[0].from, 1);
            assert_no_rejections(&stats(&db, 0));
            let records = db.list_records();
            let records: Vec<_> = records.iter().filter(|r| r.record_key == hub::INDOOR_TEMP.as_ref()).collect();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].schema_type.as_deref(), Some("temperature"));
            assert_eq!(records[0].schema_version, Some(2));
            second.producer(hub::OUTDOOR_TEMP).unwrap()
                .produce(hub::Temperature { celsius: 8.0, quality: 7 });
            assert_eq!(outdoor.recv().await.unwrap().value, hub::Temperature { celsius: 8.0, quality: 7 });
            accepted(&db, 1).await;
            assert_eq!(stats(&db, 1).accepted, 1);
            assert!(stats(&db, 1).upgraded.iter().all(|v| v.count == 0));
            db.producer(hub::RETURN_TEMP).unwrap()
                .produce(hub::Temperature { celsius: 25.0, quality: 300 });
            assert_eq!(returned.recv().await.unwrap().value, firmware::Temperature { temp: 25.0 });
            let seen = seen.lock().unwrap();
            let (_, bytes) = seen.published.iter().find(|(topic, _)| topic == "rolling/return").unwrap();
            let env = Envelope::parse(bytes).unwrap();
            assert_eq!(env.contract().version(), 1);
            assert_eq!(env.contract().shape(), firmware::Temperature::SHAPE);
            assert_eq!(seen.connects, 3);
        } => {},
        _ = tokio::time::sleep(Duration::from_secs(15)) => panic!("rolling upgrade watchdog"),
    }
}

fn assert_no_rejections(s: &InboundRouteStats) {
    assert_eq!(
        [
            s.malformed,
            s.unsupported_format,
            s.contract_mismatch,
            s.too_new,
            s.too_old,
            s.shape_mismatch,
            s.codec_mismatch,
            s.decode_error
        ],
        [0; 8]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn postcard_rolling_upgrade() {
    rolling(Postcard::<64>).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cdr_rolling_upgrade() {
    rolling(Cdr::<64>).await;
}

fn frame(contract: ContractRef, codec: PayloadCodec, payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0; 19];
    let n = HeaderWriter::new(&mut bytes)
        .contract(contract)
        .codec(codec)
        .finish()
        .unwrap();
    bytes.truncate(n);
    bytes.extend_from_slice(payload);
    bytes
}

/// Each frame crosses the embedded backend and broker before the native hub
/// admits or rejects it. A valid frame follows every rejected one, proving the
/// route stays alive. The last routes read deployed bare payloads.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rolling_rejections_and_deployed_devices() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let seen = Arc::new(Mutex::new(Seen::default()));
    let cases = [
        "old",
        "new",
        "shape",
        "contract",
        "postcard-mismatch",
        "bare-postcard",
        "bare-json",
    ];
    let receiver_keys = cases.map(aimdb_data_contracts::Key::<hub::Temperature>::new);
    let sender_keys = cases.map(aimdb_core::Name::<Vec<u8>>::new);
    let mut b = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(MqttConnector::new(url.clone()).with_client_id("rolling-policy-hub"));
    for (case, key) in cases.into_iter().zip(receiver_keys) {
        b.configure(key, |r| {
            r.buffer(BufferCfg::SingleLatest);
            let link = r.link_from(&format!("mqtt://rolling/{case}"));
            match case {
                "bare-json" => {
                    link.with_link_codec(Json)
                        .with_contract::<hub::TemperatureV1>()
                        .finish();
                }
                "bare-postcard" | "postcard-mismatch" => {
                    link.with_link_codec(Postcard::<64>)
                        .with_contract::<hub::TemperatureV1>()
                        .finish();
                }
                "old" => {
                    link.with_link_codec(Postcard::<64>)
                        .with_envelope(EnvelopeCarrier::Inline)
                        .finish();
                }
                _ => {
                    use aimdb_data_contracts::VersionedInboundExt;
                    link.with_link_codec(Postcard::<64>)
                        .with_envelope(EnvelopeCarrier::Inline)
                        .accepts_from::<1>()
                        .finish();
                }
            }
        });
    }
    let (db, runner) = b.build().await.unwrap();
    let mut mcu = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(
            MqttConnector::new(url)
                .transport(TokioNet::tcp())
                .with_client_id("rolling-policy-mcu"),
        );
    for (case, key) in cases.into_iter().zip(sender_keys) {
        mcu.configure(key, |r| {
            r.buffer(BufferCfg::SingleLatest)
                .link_to(&format!("mqtt://rolling/{case}"))
                .with_serializer(|_, bytes: &Vec<u8>| Ok(bytes.clone()))
                .finish();
        });
    }
    let (mcu, mcu_runner) = mcu.build().await.unwrap();
    let mut readers: Vec<_> = receiver_keys
        .into_iter()
        .map(|key| db.consumer(key).unwrap().subscribe())
        .collect();
    let v1 = firmware::Temperature { temp: 21.5 };
    let v1_payload = Postcard::<64>.encode_payload_owned(&v1).unwrap();
    let reference = ContractRef::new(name_id("temperature"), 1, firmware::Temperature::SHAPE);
    let inline = frame(reference, PayloadCodec::Postcard, &v1_payload);
    let current = hub::Temperature {
        celsius: 21.5,
        quality: 1,
    };
    let current_payload = Postcard::<64>.encode_payload_owned(&current).unwrap();
    let current_ref = ContractRef::new(name_id("temperature"), 2, hub::Temperature::SHAPE);
    let valid_current = frame(current_ref, PayloadCodec::Postcard, &current_payload);
    let changed = rolling_contracts_changed::TemperatureV2 {
        celsius: 21.5,
        quality: 1,
        unexpected: 42,
    };
    assert_ne!(
        rolling_contracts_changed::TemperatureV2::SHAPE,
        hub::Temperature::SHAPE
    );
    let bad = [
        inline.clone(),
        frame(
            ContractRef::new(name_id("temperature"), 3, None),
            PayloadCodec::Postcard,
            &v1_payload,
        ),
        frame(
            ContractRef::new(
                name_id("temperature"),
                2,
                rolling_contracts_changed::TemperatureV2::SHAPE,
            ),
            PayloadCodec::Postcard,
            &Postcard::<64>.encode_payload_owned(&changed).unwrap(),
        ),
        frame(
            ContractRef::new(name_id("humidity"), 1, hub::Humidity::SHAPE),
            PayloadCodec::Postcard,
            &Postcard::<64>
                .encode_payload_owned(&hub::Humidity { percent: 55.0 })
                .unwrap(),
        ),
        inline.clone(),
    ];
    tokio::select! {
        _ = runner.run() => panic!("hub stopped"),
        _ = mcu_runner.run() => panic!("MCU stopped"),
        _ = common::forwarding_broker(listener, seen.clone()) => panic!("broker stopped"),
        _ = async {
            subscribed(&seen, cases.len()).await;
            for (i, bytes) in bad.into_iter().enumerate() {
                mcu.producer(sender_keys[i]).unwrap().produce(bytes);
                loop {
                    let s = stats(&db, i);
                    let rejected = [s.too_old, s.too_new, s.shape_mismatch, s.contract_mismatch, s.decode_error];
                    if rejected[i] == 1 {
                        assert_eq!(rejected.iter().sum::<u64>(), 1);
                        assert_eq!(s.accepted, 0);
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                // Recover on the very same route, respecting its carrier.
                mcu.producer(sender_keys[i]).unwrap().produce(if i == 4 { v1_payload.clone() } else { valid_current.clone() });
                assert_eq!(readers[i].recv().await.unwrap().value, current);
                assert_eq!(accepted(&db, i).await.accepted, 1);
            }
            for (i, bytes) in [(5, v1_payload.clone()), (6, br#"{"schema_version":999,"temp":21.5}"#.to_vec())] {
                mcu.producer(sender_keys[i]).unwrap().produce(bytes);
                assert_eq!(readers[i].recv().await.unwrap().value, current);
                let s = accepted(&db, i).await;
                assert_eq!(s.accepted, 1);
                assert_eq!(s.upgraded[0].count, 1);
                assert_no_rejections(&s);
            }
        } => {},
        _ = tokio::time::sleep(Duration::from_secs(15)) => panic!("rolling rejection watchdog: {:?}", seen.lock().unwrap().published),
    }
}
