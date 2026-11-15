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
/// Link config key selecting the carrier (`"inline"` or `"bound"`).
///
/// Core consumes it at registration: it never reaches a connector's config.
/// An inline carrier is passed on as
/// [`WIRE_ENVELOPE_KEY`](crate::connector::WIRE_ENVELOPE_KEY) instead.
pub const ENVELOPE_KEY: &str = "envelope";

impl WireOptions {
    /// Applies the last [`ENVELOPE_KEY`] entry, then removes every such entry.
    /// The carrier is core's concern; transports keep their own config namespace.
    pub(crate) fn take_carrier(
        &mut self,
        config: &mut alloc::vec::Vec<(alloc::string::String, alloc::string::String)>,
    ) -> Result<(), alloc::string::String> {
        if let Some((_, value)) = config.iter().rev().find(|(key, _)| key == ENVELOPE_KEY) {
            self.carrier = match value.as_str() {
                "inline" => EnvelopeCarrier::Inline,
                "bound" => EnvelopeCarrier::Bound,
                _ => return Err("envelope must be 'inline' or 'bound'".into()),
            };
        }
        // The reserved framing keys are recorded at registration only.
        config.retain(|(key, _)| {
            key != ENVELOPE_KEY
                && key != crate::connector::WIRE_ENVELOPE_KEY
                && key != crate::connector::WIRE_VERSION_KEY
        });
        Ok(())
    }

    /// Records an inline carrier under
    /// [`WIRE_ENVELOPE_KEY`](crate::connector::WIRE_ENVELOPE_KEY), so the
    /// connector sees the framing the finalized link puts on the wire.
    pub(crate) fn record_carrier(
        &self,
        config: &mut alloc::vec::Vec<(alloc::string::String, alloc::string::String)>,
    ) {
        if self.carrier == EnvelopeCarrier::Inline {
            config.push((crate::connector::WIRE_ENVELOPE_KEY.into(), "inline".into()));
        }
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
