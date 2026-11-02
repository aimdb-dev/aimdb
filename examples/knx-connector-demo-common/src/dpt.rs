//! The demo's KNX payloads: DPT bytes to records and back, written once.
//!
//! A group telegram carries only the value. The decoders therefore take what
//! the payload does not: where a reading came from, or which group address a
//! state belongs to. Pass them to `with_deserializer` / `with_serializer` on a
//! link from the KNX connector's handle:
//!
//! ```rust,ignore
//! let knx = builder.connect(KnxConnector::new(..));
//!
//! builder.configure::<TemperatureReading>(TemperatureKey::LivingRoom, |reg| {
//!     reg.buffer(BufferCfg::SingleLatest).input(
//!         InboundLink::on(knx, TemperatureKey::LivingRoom.link_address().unwrap())
//!             .with_deserializer(dpt::temperature_at("Living Room")),
//!     );
//! });
//! ```
//!
//! These records are not `Linkable` contracts: their payload alone does not
//! make a record, so they use the raw (de)serializers on a handle link.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use aimdb_core::RuntimeContext;
use aimdb_core::connector::SerializeError;
use aimdb_knx_connector::dpt::{Dpt1, Dpt9, DptDecode, DptEncode};

use crate::types::{LightControl, LightState, TemperatureReading};

/// Decodes a DPT 9.001 temperature as a reading from `location`. A payload
/// that does not decode reads as 0.0 °C.
pub fn temperature_at(
    location: &'static str,
) -> impl Fn(RuntimeContext, &[u8]) -> Result<TemperatureReading, String> + Send + Sync + 'static {
    move |_ctx, data| {
        let celsius = Dpt9::Temperature.decode(data).unwrap_or(0.0);
        Ok(TemperatureReading::new(location, celsius))
    }
}

/// Decodes a DPT 1.001 switch as the state of `group_address`. A payload
/// that does not decode reads as off.
pub fn light_state_at(
    group_address: &'static str,
) -> impl Fn(RuntimeContext, &[u8]) -> Result<LightState, String> + Send + Sync + 'static {
    move |_ctx, data| {
        let is_on = Dpt1::Switch.decode(data).unwrap_or(false);
        Ok(LightState::new(group_address, is_on))
    }
}

/// Encodes a light command as DPT 1.001.
pub fn switch(_ctx: RuntimeContext, control: &LightControl) -> Result<Vec<u8>, SerializeError> {
    let mut buf = [0u8; 1];
    let len = Dpt1::Switch.encode(control.is_on, &mut buf).unwrap_or(0);
    Ok(buf[..len].to_vec())
}
