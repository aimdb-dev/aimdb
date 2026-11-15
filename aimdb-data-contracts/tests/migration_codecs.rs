//! Codec-independent migration without in-band version fields or probe parses.
#![cfg(feature = "migratable")]

use aimdb_data_contracts::{
    migration_chain, ContractShape, MigrationChain, MigrationError, MigrationStep, PayloadDecoder,
    PayloadEncoder, Reaches, SchemaType, VersionOf,
};
use core::cell::Cell;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct V1 {
    temp: f32,
}
impl SchemaType for V1 {
    const NAME: &'static str = "temperature";
}
impl ContractShape for V1 {
    const SHAPE: Option<u64> = Some(0x1234);
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct V2 {
    celsius: f32,
    quality: u32,
}
impl SchemaType for V2 {
    const NAME: &'static str = "temperature";
    const VERSION: u32 = 2;
}
impl ContractShape for V2 {
    const SHAPE: Option<u64> = Some(0x5678);
}
struct Step;
impl MigrationStep for Step {
    type Older = V1;
    type Newer = V2;
    const FROM_VERSION: u32 = 1;
    const TO_VERSION: u32 = 2;
    fn up(v: V1) -> Result<V2, MigrationError> {
        if v.temp < -273.15 {
            return Err(MigrationError::ConversionFailed("below absolute zero"));
        }
        Ok(V2 {
            celsius: v.temp,
            quality: 1,
        })
    }
    fn down(v: V2) -> Result<V1, MigrationError> {
        if v.quality == 0 {
            return Err(MigrationError::ConversionFailed("invalid quality"));
        }
        Ok(V1 { temp: v.celsius })
    }
}
migration_chain! { type Current = V2; steps { Step: V1 => V2 } }

#[cfg(any(
    feature = "linkable-json",
    feature = "linkable-postcard",
    feature = "linkable-cdr"
))]
struct CountingDecoder<C> {
    codec: C,
    calls: Cell<usize>,
}
#[cfg(any(
    feature = "linkable-json",
    feature = "linkable-postcard",
    feature = "linkable-cdr"
))]
impl<C: PayloadDecoder> PayloadDecoder for CountingDecoder<C> {
    fn decode_payload<T: serde::de::DeserializeOwned>(
        &self,
        bytes: &[u8],
    ) -> Result<(T, usize), String> {
        self.calls.set(self.calls.get() + 1);
        self.codec.decode_payload(bytes)
    }
}

#[cfg(any(
    feature = "linkable-json",
    feature = "linkable-postcard",
    feature = "linkable-cdr"
))]
fn exercise<C: PayloadDecoder + PayloadEncoder>(codec: C) {
    let decoder = CountingDecoder {
        codec,
        calls: Cell::new(0),
    };
    let mut bytes = [0; 128];
    let n = decoder
        .codec
        .encode_payload(&V1 { temp: 21.5 }, &mut bytes)
        .unwrap();
    let current = V2::migrate_from(1, &bytes[..n], &decoder).unwrap();
    assert_eq!(
        current,
        V2 {
            celsius: 21.5,
            quality: 1
        }
    );
    assert_eq!(decoder.calls.get(), 1);
    let n = current.migrate_into(1, &decoder.codec, &mut bytes).unwrap();
    assert_eq!(
        decoder.codec.decode_payload::<V1>(&bytes[..n]).unwrap(),
        (V1 { temp: 21.5 }, n)
    );
    let n = current.migrate_into(2, &decoder.codec, &mut bytes).unwrap();
    assert_eq!(V2::migrate_from(2, &bytes[..n], &decoder).unwrap(), current);
    assert_eq!(decoder.calls.get(), 2);
    assert!(matches!(
        V2::migrate_from(2, &[], &decoder),
        Err(MigrationError::DeserializationFailed { .. })
    ));
    assert_eq!(decoder.calls.get(), 3);
    for (version, error) in [
        (
            0,
            MigrationError::VersionTooOld {
                target: 0,
                minimum: 1,
            },
        ),
        (
            3,
            MigrationError::VersionTooNew {
                source: 3,
                current: 2,
            },
        ),
    ] {
        assert_eq!(V2::migrate_from(version, &[], &decoder), Err(error.clone()));
        assert_eq!(
            current.migrate_into(version, &decoder.codec, &mut []),
            Err(error)
        );
    }
    assert_eq!(decoder.calls.get(), 3, "invalid versions must not decode");
    let n = CurrentOnly { n: 42 }
        .migrate_into(7, &decoder.codec, &mut bytes)
        .unwrap();
    assert_eq!(
        CurrentOnly::migrate_from(7, &bytes[..n], &decoder).unwrap(),
        CurrentOnly { n: 42 }
    );

    assert_eq!(
        current.migrate_into(1, &decoder.codec, &mut []),
        Err(MigrationError::BufferTooSmall)
    );
    assert_eq!(
        current.migrate_into(2, &decoder.codec, &mut []),
        Err(MigrationError::BufferTooSmall)
    );
    let n = decoder
        .codec
        .encode_payload(&V1 { temp: -300.0 }, &mut bytes)
        .unwrap();
    assert_eq!(
        V2::migrate_from(1, &bytes[..n], &decoder),
        Err(MigrationError::ConversionFailed("below absolute zero"))
    );
    assert_eq!(
        V2 {
            celsius: 0.0,
            quality: 0
        }
        .migrate_into(1, &decoder.codec, &mut bytes),
        Err(MigrationError::ConversionFailed("invalid quality"))
    );
}

#[cfg(feature = "linkable-json")]
#[test]
fn json_has_no_version_field_or_probe() {
    exercise(aimdb_data_contracts::link_codecs::Json);
}
#[cfg(feature = "linkable-postcard")]
#[test]
fn postcard_migrates_in_both_directions() {
    exercise(aimdb_data_contracts::link_codecs::Postcard::<128>);
}
#[cfg(feature = "linkable-cdr")]
#[test]
fn cdr_migrates_in_both_directions() {
    exercise(aimdb_data_contracts::link_codecs::Cdr::<128>);
}

// No Clone impl: current-only sends must work by reference.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct CurrentOnly {
    n: u32,
}
impl SchemaType for CurrentOnly {
    const NAME: &'static str = "singleton";
    const VERSION: u32 = 7;
}
impl ContractShape for CurrentOnly {}
migration_chain! { type Current = CurrentOnly; steps {} }

#[test]
fn historical_shapes_and_version_proofs() {
    fn reaches<T: Reaches<1> + Reaches<2>>() {}
    fn version<U: VersionOf<V2>>() {}
    reaches::<V2>();
    version::<V1>();
    version::<V2>();
    fn singleton<T: Reaches<7> + VersionOf<CurrentOnly>>() {}
    singleton::<CurrentOnly>();
    assert_eq!(V2::MIN_VERSION, 1);
    assert_eq!(V2::shape_of(0), None);
    assert_eq!(V2::shape_of(1), Some(0x1234));
    assert_eq!(V2::shape_of(2), Some(0x5678));
    assert_eq!(V2::shape_of(3), None);
    assert_eq!(CurrentOnly::MIN_VERSION, 7);
    assert_eq!(CurrentOnly::shape_of(7), None);
}

#[test]
fn current_only_dispatches_without_clone() {
    struct NeverDecode;
    impl PayloadDecoder for NeverDecode {
        fn decode_payload<T: serde::de::DeserializeOwned>(
            &self,
            _: &[u8],
        ) -> Result<(T, usize), String> {
            panic!("invalid versions must be rejected before decoding")
        }
    }
    struct Encoder(Cell<usize>);
    impl PayloadEncoder for Encoder {
        fn encode_payload<T: Serialize>(
            &self,
            _: &T,
            _: &mut [u8],
        ) -> Result<usize, aimdb_core::connector::SerializeError> {
            self.0.set(self.0.get() + 1);
            Ok(4)
        }
    }
    let value = CurrentOnly { n: 42 };
    let encoder = Encoder(Cell::new(0));
    assert_eq!(value.migrate_into(7, &encoder, &mut [0; 4]), Ok(4));
    assert_eq!(encoder.0.get(), 1);
    assert_eq!(
        value.migrate_into(6, &encoder, &mut []),
        Err(MigrationError::VersionTooOld {
            target: 6,
            minimum: 7
        })
    );
    assert_eq!(encoder.0.get(), 1);
    assert!(matches!(
        CurrentOnly::migrate_from(6, &[], &NeverDecode),
        Err(MigrationError::VersionTooOld { .. })
    ));
}
