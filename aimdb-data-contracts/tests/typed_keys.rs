//! Typed declarations own record schema, independently of links (063 R3).
#![cfg(all(feature = "migratable", feature = "linkable-postcard"))]
#[path = "fixtures/admission_contracts.rs"]
mod contracts;
use aimdb_core::executor::test_support::NoopRuntimeOps;
use aimdb_core::{AimDbBuilder, Name, RecordKey, TypedKey};
use aimdb_data_contracts::Key;
use contracts::{V1, V2};
use std::sync::Arc;
const READING: Key<V2> = Key::new("temperature.indoor");
const SECOND: Key<V2> = Key::new("temperature.outdoor");
const PLAIN: Name<V2> = Name::new("internal");
fn builder() -> AimDbBuilder {
    AimDbBuilder::new().runtime(Arc::new(NoopRuntimeOps))
}
#[test]
fn keys_report_schema_without_links_and_names_do_not() {
    futures::executor::block_on(async {
        let mut b = builder();
        b.configure(READING, |_| {})
            .configure(SECOND, |_| {})
            .configure(PLAIN, |_| {});
        let (db, _) = b.build().await.unwrap();
        for key in [READING, SECOND] {
            let node = db.inner().dependency_graph().node(key.as_str()).unwrap();
            assert_eq!(node.schema.as_ref().unwrap().name, "temperature");
            assert_eq!(node.schema.as_ref().unwrap().version, 2);
            let metadata = db
                .list_records()
                .into_iter()
                .find(|m| m.record_key == key.as_str())
                .unwrap();
            assert_eq!(metadata.schema_version, Some(2));
        }
        assert!(db
            .inner()
            .dependency_graph()
            .node(PLAIN.as_str())
            .unwrap()
            .schema
            .is_none());
        assert!(db.consumer(READING).is_err()); // No buffer configured.
        let runtime_name = String::from("temperature.indoor");
        let lookup = Name::<V2>::new(&runtime_name);
        assert!(db.latest(lookup).unwrap().is_none());
    });
}
#[test]
fn shape_free_names_and_keys_are_copy_without_payload_eq_or_hash() {
    let with_address = READING.with_link("test://reading");
    assert_eq!(READING, with_address);
    assert_eq!(with_address.link_address(), Some("test://reading"));
    assert_eq!(<Key<V2> as TypedKey>::SCHEMA, Some(("temperature", 2)));
    assert_eq!(<Name<V2> as TypedKey>::SCHEMA, None);
}
#[test]
fn conflicting_types_are_runtime_errors_even_with_typed_declarations() {
    futures::executor::block_on(async {
        const WRONG: Key<V1> = Key::new("temperature.indoor");
        let mut b = builder();
        b.configure(READING, |_| {}).configure(WRONG, |_| {});
        assert!(b.build().await.is_err());
        let mut b = builder();
        b.configure(READING, |_| {});
        let (db, _) = b.build().await.unwrap();
        assert!(matches!(
            db.latest(WRONG),
            Err(aimdb_core::DbError::TypeMismatch { .. })
        ));
    });
}

#[derive(aimdb_derive::RecordKey, Clone, Copy, Debug, PartialEq, Eq)]
#[record_key(contract=V2,prefix="station.")]
enum Stations {
    #[key = "one"]
    #[link_address = "test://one"]
    One,
    #[key = "two"]
    Two,
}
#[derive(aimdb_derive::RecordKey, Clone, Copy, Debug, PartialEq, Eq)]
#[record_key(payload=u64,prefix="counter.")]
enum Counters {
    #[key = "in"]
    In,
}
#[test]
fn derived_keys_infer_contract_and_plain_payloads() {
    futures::executor::block_on(async {
        let mut b = builder();
        b.configure(Stations::One, |_| {})
            .configure(Stations::Two, |_| {})
            .configure(Counters::In, |_| {});
        let (db, _) = b.build().await.unwrap();
        assert_eq!(Stations::One.link_address(), Some("test://one"));
        assert_eq!(
            db.inner()
                .dependency_graph()
                .node("station.one")
                .unwrap()
                .schema
                .as_ref()
                .unwrap()
                .version,
            2
        );
        assert!(db
            .inner()
            .dependency_graph()
            .node("counter.in")
            .unwrap()
            .schema
            .is_none());
    });
}

#[test]
fn borrowed_lookup_and_interned_registration_share_identity() {
    futures::executor::block_on(async {
        let name = String::from("runtime.temperature");
        let key = Key::<V2>::interned(&name);
        let lookup = Name::<V2>::new(&name);
        let mut b = builder();
        b.configure(key, |_| {});
        let (db, _) = b.build().await.unwrap();
        assert!(db.latest(lookup).unwrap().is_none());
        assert_eq!(
            db.inner()
                .dependency_graph()
                .node(&name)
                .unwrap()
                .schema
                .as_ref()
                .unwrap()
                .version,
            2
        );
        let plain = Name::<V2>::new(READING.as_str());
        let mut b = builder();
        b.configure(READING, |_| {}).configure(plain, |_| {});
        assert!(b.build().await.is_err());
    });
}
