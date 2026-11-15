#![no_std]
//! An independently compiled revision of the fleet's contracts.

use aimdb_data_contracts::{migration_chain, SchemaType};
use serde::{Deserialize, Serialize};

pub const INDOOR_TEMP: aimdb_data_contracts::Key<Temperature> =
    aimdb_data_contracts::Key::new("temp.indoor").with_link(INDOOR_URL);
pub const INDOOR_URL: &str = "mqtt://rolling/indoor";
pub const RETURN_TEMP: aimdb_data_contracts::Key<Temperature> =
    aimdb_data_contracts::Key::new("temp.return").with_link(RETURN_URL);
pub const RETURN_URL: &str = "mqtt://rolling/return";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TemperatureV1 {
    pub temp: f32,
}
impl SchemaType for TemperatureV1 {
    const NAME: &'static str = "temperature";
    const VERSION: u32 = 1;
}

use aimdb_data_contracts::{MigrationError, MigrationStep};
pub const OUTDOOR_TEMP: aimdb_data_contracts::Key<Temperature> =
    aimdb_data_contracts::Key::new("temp.outdoor").with_link(OUTDOOR_URL);
pub const OUTDOOR_URL: &str = "mqtt://rolling/outdoor";
pub type Temperature = TemperatureV2;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TemperatureV2 {
    pub celsius: f32,
    pub quality: u32,
}
impl SchemaType for TemperatureV2 {
    const NAME: &'static str = "temperature";
    const VERSION: u32 = 2;
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Humidity {
    pub percent: f32,
}
impl SchemaType for Humidity {
    const NAME: &'static str = "humidity";
}

pub struct V1ToV2;
impl MigrationStep for V1ToV2 {
    type Older = TemperatureV1;
    type Newer = TemperatureV2;
    const FROM_VERSION: u32 = 1;
    const TO_VERSION: u32 = 2;
    fn up(v: TemperatureV1) -> Result<TemperatureV2, MigrationError> {
        Ok(TemperatureV2 {
            celsius: v.temp,
            quality: 1,
        })
    }
    fn down(v: TemperatureV2) -> Result<TemperatureV1, MigrationError> {
        Ok(TemperatureV1 { temp: v.celsius })
    }
}
aimdb_data_contracts::catalog! { contracts: [TemperatureV1, TemperatureV2, Humidity], helpers: [], }
migration_chain! { type Current = TemperatureV2; steps { V1ToV2: TemperatureV1 => TemperatureV2 } }

pub fn receive<
    C: aimdb_data_contracts::LinkCodec<Temperature> + 'static,
    S: aimdb_core::SchemaState,
>(
    r: &mut aimdb_core::RecordRegistrar<'_, Temperature, S>,
    codec: C,
) {
    use aimdb_data_contracts::{
        EnvelopeCarrier, EnvelopeLinkExt, LinkCodecBuilderExt, VersionedInboundExt,
    };
    r.link_from_key()
        .with_link_codec(codec)
        .with_envelope(EnvelopeCarrier::Inline)
        .accepts_from::<1>()
        .finish();
}

pub fn send_to_firmware<
    C: aimdb_data_contracts::LinkCodec<Temperature> + 'static,
    S: aimdb_core::SchemaState,
>(
    r: &mut aimdb_core::RecordRegistrar<'_, Temperature, S>,
    codec: C,
) {
    use aimdb_data_contracts::{
        EnvelopeCarrier, EnvelopeLinkExt, LinkCodecBuilderExt, VersionedOutboundExt,
    };
    r.link_to_key()
        .with_link_codec(codec)
        .with_envelope(EnvelopeCarrier::Inline)
        .sends_as::<1>()
        .finish();
}
