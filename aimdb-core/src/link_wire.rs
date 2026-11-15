//! Registration-time hooks shared with the data-contracts layer.

use crate::envelope::{ContractRef, PayloadCodec};

/// Carrier of a record value. Bare/static binding remains the default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EnvelopeCarrier {
    #[default]
    Bound,
    Inline,
}

/// Implementation detail of contract-aware builder extensions.
#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub struct WireOptions {
    pub carrier: EnvelopeCarrier,
    pub binding: Option<ContractRef>,
    pub accepts_from: Option<u32>,
    pub sends_as: Option<u32>,
    pub include_shape: bool,
}
impl Default for WireOptions {
    fn default() -> Self {
        Self {
            carrier: EnvelopeCarrier::Bound,
            binding: None,
            accepts_from: None,
            sends_as: None,
            include_shape: true,
        }
    }
}
impl WireOptions {
    pub(crate) fn read_carrier(
        &mut self,
        config: &[(alloc::string::String, alloc::string::String)],
    ) -> Result<(), alloc::string::String> {
        if let Some((_, value)) = config.iter().rev().find(|(key, _)| key == "envelope") {
            self.carrier = match value.as_str() {
                "inline" => EnvelopeCarrier::Inline,
                "bound" => EnvelopeCarrier::Bound,
                _ => return Err("envelope must be 'inline' or 'bound'".into()),
            };
        }
        Ok(())
    }
}

/// A stateless builtin codec descriptor, or an explicitly captured custom codec.
/// This is consumed at registration, never matched in the per-message path.
#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub struct WireCodecInfo {
    pub codec: PayloadCodec,
    pub capacity: Option<usize>,
    pub custom: bool,
}
