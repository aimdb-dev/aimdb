//! Contract declarations finalized once, after all link options and codec selection.

use crate::{
    admit_frame, ContractShape, LinkCodec, LinkCodecBuilderExt, LinkContract, MigrationChain,
    PayloadDecoder, PayloadEncoder, Reaches, SchemaType, VersionOf,
};
use aimdb_core::connector::{SerializeError, WIRE_FORMAT_KEY, WIRE_VERSION_KEY};
use aimdb_core::envelope::{ContractRef, HeaderWriter};
use aimdb_core::link_wire::{WireCodecInfo, WireOptions};
use aimdb_core::{
    EnvelopeCarrier, InboundLink, InboundLinkBuilder, OutboundLink, OutboundLinkBuilder,
};
use alloc::{format, string::String, vec::Vec};
use core::{
    cell::{Cell, RefCell},
    fmt::Debug,
};

type WireResult<T> = Result<T, String>;

/// Explicit carrier and static binding on contract links. Options may precede
/// or follow codec selection. Both attached and detached builders keep their type.
pub trait EnvelopeLinkExt<T: MigrationChain>: Sized {
    fn with_envelope(self, carrier: EnvelopeCarrier) -> Self;
    fn with_contract<U: SchemaType + ContractShape + VersionOf<T>>(self) -> Self;
}
pub trait VersionedInboundExt<T: MigrationChain>: Sized {
    /// Only inline links accept several versions. Invalid combinations fail build().
    fn accepts_from<const V: u32>(self) -> Self
    where
        T: Reaches<V>;
}
pub trait VersionedOutboundExt<T: MigrationChain>: Sized {
    fn sends_as<const V: u32>(self) -> Self
    where
        T: Reaches<V>;
    fn without_shape(self) -> Self;
}
/// Explicitly opt a custom generic codec into versioned admission and encoding.
/// Builtin Json/Postcard/CDR markers also work with ordinary `with_link_codec`.
pub trait VersionedCodecBuilderExt<T: MigrationChain>: Sized {
    fn with_versioned_codec<C>(self, codec: C) -> Self
    where
        C: LinkCodec<T> + PayloadDecoder + PayloadEncoder;
}

fn validate<T: MigrationChain>(options: WireOptions, inbound: bool) -> WireResult<u32> {
    if options.carrier == EnvelopeCarrier::Inline && options.binding.is_some() {
        return Err("with_contract is only valid on a bound link".into());
    }
    if let Some(binding) = options.binding {
        if binding.name_id() != crate::name_id(T::NAME)
            || binding.version() < T::MIN_VERSION
            || binding.version() > T::VERSION
        {
            return Err("static binding is outside this contract's migration chain".into());
        }
    }
    if inbound {
        if options.accepts_from.is_some() && options.carrier != EnvelopeCarrier::Inline {
            return Err(
                "accepts_from requires an inline envelope and cannot combine with with_contract"
                    .into(),
            );
        }
        let minimum = options.accepts_from.unwrap_or(T::VERSION);
        if minimum < T::MIN_VERSION || minimum > T::VERSION {
            return Err("accepted minimum is outside the migration chain".into());
        }
        Ok(minimum)
    } else {
        if let (Some(binding), Some(sent)) = (options.binding, options.sends_as) {
            if binding.version() != sent {
                return Err("with_contract and sends_as select different versions".into());
            }
        }
        let sent = options
            .sends_as
            .or(options.binding.map(|c| c.version()))
            .unwrap_or(T::VERSION);
        if sent < T::MIN_VERSION || sent > T::VERSION {
            return Err("send version is outside the migration chain".into());
        }
        Ok(sent)
    }
}

fn inbound<T, C>(link: InboundLink<T>, codec: C, info: WireCodecInfo) -> WireResult<InboundLink<T>>
where
    T: MigrationChain + Send + Sync + Clone + Debug + 'static,
    C: PayloadDecoder + Clone + Send + Sync + 'static,
{
    let options = link.wire_options();
    let minimum = validate::<T>(options, true)?;
    let binding = if options.carrier == EnvelopeCarrier::Bound {
        Some(options.binding.unwrap_or(ContractRef::new(
            crate::name_id(T::NAME),
            T::VERSION,
            T::shape_of(T::VERSION),
        )))
    } else {
        None
    };
    let versions: Vec<u32> = if let Some(binding) = binding {
        if binding.version() < T::VERSION {
            alloc::vec![binding.version()]
        } else {
            Vec::new()
        }
    } else {
        (minimum..T::VERSION).collect()
    };
    let policy = LinkContract::<T, _>::configured(info.codec, codec, binding, Some(minimum));
    let link = link
        .clear_deserializers()
        .with_admitting_deserializer(&versions, move |_, _, bytes| admit_frame(bytes, &policy))
        .with_schema(T::NAME, T::VERSION)
        .with_wire_codec_info(Some(info));
    // A bound link reads one version; tell the connector when it is not the
    // current one.
    let link = match binding {
        Some(binding) if binding.version() != T::VERSION => {
            link.with_config(WIRE_VERSION_KEY, &format!("{}", binding.version()))
        }
        _ => link,
    };
    Ok(record_format(link, info.codec))
}

fn record_format<T, R>(
    link: InboundLinkBuilder<T, R>,
    codec: aimdb_core::envelope::PayloadCodec,
) -> InboundLinkBuilder<T, R>
where
    T: Send + Sync + Clone + Debug + 'static,
{
    match codec {
        aimdb_core::envelope::PayloadCodec::Json => link.with_config(WIRE_FORMAT_KEY, "json"),
        aimdb_core::envelope::PayloadCodec::Postcard => {
            link.with_config(WIRE_FORMAT_KEY, "postcard")
        }
        aimdb_core::envelope::PayloadCodec::Cdr => link.with_config(WIRE_FORMAT_KEY, "cdr"),
        _ => link,
    }
}

fn migration_error(error: crate::MigrationError) -> SerializeError {
    match error {
        crate::MigrationError::BufferTooSmall => SerializeError::BufferTooSmall,
        _ => SerializeError::InvalidData,
    }
}
fn header(
    out: &mut [u8],
    contract: ContractRef,
    info: WireCodecInfo,
    inline: bool,
) -> Result<usize, SerializeError> {
    if !inline {
        return Ok(0);
    }
    HeaderWriter::new(out)
        .contract(contract)
        .codec(info.codec)
        .finish()
        .map_err(|error| match error {
            aimdb_core::envelope::EnvelopeError::BufferTooSmall => SerializeError::BufferTooSmall,
            _ => SerializeError::InvalidData,
        })
}
fn encode_into<T: MigrationChain, C: PayloadEncoder>(
    value: &T,
    codec: &C,
    version: u32,
    contract: ContractRef,
    info: WireCodecInfo,
    inline: bool,
    out: &mut [u8],
) -> Result<usize, SerializeError> {
    let offset = header(out, contract, info, inline)?;
    let n = value
        .migrate_into(version, codec, &mut out[offset..])
        .map_err(migration_error)?;
    if n > out.len() - offset {
        return Err(SerializeError::InvalidData);
    }
    Ok(offset + n)
}

// Invoke the downgrade exactly once on the owned fallback, then let the
// codec append the historical value right after the header (no fixed size
// ceiling, and the payload is never moved to make room for the header).
struct OwnedEncoder<'a, C> {
    codec: &'a C,
    out: RefCell<Vec<u8>>,
    used: Cell<bool>,
}
impl<C: PayloadEncoder> PayloadEncoder for OwnedEncoder<'_, C> {
    fn encode_payload<U: serde::Serialize>(
        &self,
        value: &U,
        _: &mut [u8],
    ) -> Result<usize, SerializeError> {
        if self.used.replace(true) {
            return Err(SerializeError::InvalidData);
        }
        self.codec
            .encode_payload_append(value, &mut self.out.borrow_mut())?;
        Ok(0)
    }
}
fn encode_owned<T: MigrationChain, C: PayloadEncoder>(
    value: &T,
    codec: &C,
    version: u32,
    contract: ContractRef,
    info: WireCodecInfo,
    inline: bool,
) -> Result<Vec<u8>, SerializeError> {
    let mut bytes = [0; 19];
    let n = header(&mut bytes, contract, info, inline)?;
    let capture = OwnedEncoder {
        codec,
        out: RefCell::new(bytes[..n].to_vec()),
        used: Cell::new(false),
    };
    value
        .migrate_into(version, &capture, &mut [])
        .map_err(migration_error)?;
    if !capture.used.get() {
        return Err(SerializeError::InvalidData);
    }
    Ok(capture.out.into_inner())
}

fn outbound<T, C>(
    link: OutboundLink<T>,
    codec: C,
    info: WireCodecInfo,
) -> WireResult<OutboundLink<T>>
where
    T: MigrationChain + Send + Sync + Clone + Debug + 'static,
    C: PayloadEncoder + Clone + Send + Sync + 'static,
{
    let options = link.wire_options();
    let version = validate::<T>(options, false)?;
    let inline = options.carrier == EnvelopeCarrier::Inline;
    let contract = ContractRef::new(
        crate::name_id(T::NAME),
        version,
        if options.include_shape {
            T::shape_of(version)
        } else {
            None
        },
    );
    let capacity = info
        .capacity
        .map(|n| {
            n.checked_add(if inline { 19 } else { 0 })
                .ok_or_else(|| String::from("envelope scratch capacity overflow"))
        })
        .transpose()?;
    let scratch_codec = codec.clone();
    let link = link.with_serializer(move |_, value| {
        encode_owned(value, &codec, version, contract, info, inline)
    });
    let link = match capacity {
        Some(capacity) => link.with_serializer_into(capacity, move |_, value, out| {
            encode_into(value, &scratch_codec, version, contract, info, inline, out)
        }),
        None => link.clear_serializer_into(),
    }
    .with_schema(T::NAME, T::VERSION)
    .with_wire_codec_info(Some(info));
    let link = if version != T::VERSION {
        link.with_config(WIRE_VERSION_KEY, &format!("{version}"))
    } else {
        link
    };
    Ok(match info.codec {
        aimdb_core::envelope::PayloadCodec::Json => link.with_config(WIRE_FORMAT_KEY, "json"),
        aimdb_core::envelope::PayloadCodec::Postcard => {
            link.with_config(WIRE_FORMAT_KEY, "postcard")
        }
        aimdb_core::envelope::PayloadCodec::Cdr => link.with_config(WIRE_FORMAT_KEY, "cdr"),
        _ => link,
    })
}

fn builtin_inbound<T>(link: InboundLink<T>) -> WireResult<InboundLink<T>>
where
    T: MigrationChain + Send + Sync + Clone + Debug + 'static,
{
    let info = link.wire_codec_info().ok_or_else(|| String::from("select a builtin codec with with_link_codec, or a generic custom codec with with_versioned_codec"))?;
    if info.custom {
        return Err("custom versioned codec strategy was replaced".into());
    }
    match info.codec {
        #[cfg(feature = "linkable-json")]
        aimdb_core::envelope::PayloadCodec::Json => inbound(link, crate::link_codecs::Json, info),
        #[cfg(feature = "linkable-postcard")]
        aimdb_core::envelope::PayloadCodec::Postcard => {
            inbound(link, crate::link_codecs::Postcard::<1>, info)
        }
        #[cfg(feature = "linkable-cdr")]
        aimdb_core::envelope::PayloadCodec::Cdr => {
            inbound(link, crate::link_codecs::Cdr::<1>, info)
        }
        _ => Err(format!(
            "codec {:?} has no generic decoder in this build",
            info.codec
        )),
    }
}
fn builtin_outbound<T>(link: OutboundLink<T>) -> WireResult<OutboundLink<T>>
where
    T: MigrationChain + Send + Sync + Clone + Debug + 'static,
{
    let info = link.wire_codec_info().ok_or_else(|| String::from("select a builtin codec with with_link_codec, or a generic custom codec with with_versioned_codec"))?;
    if info.custom {
        return Err("custom versioned codec strategy was replaced".into());
    }
    match info.codec {
        #[cfg(feature = "linkable-json")]
        aimdb_core::envelope::PayloadCodec::Json => outbound(link, crate::link_codecs::Json, info),
        #[cfg(feature = "linkable-postcard")]
        aimdb_core::envelope::PayloadCodec::Postcard => {
            outbound(link, crate::link_codecs::Postcard::<1>, info)
        }
        #[cfg(feature = "linkable-cdr")]
        aimdb_core::envelope::PayloadCodec::Cdr => {
            outbound(link, crate::link_codecs::Cdr::<1>, info)
        }
        _ => Err(format!(
            "codec {:?} has no generic encoder in this build",
            info.codec
        )),
    }
}

macro_rules! envelope_ext {
    ($builder:ident, $finalize:ident) => {
        impl<T, R> EnvelopeLinkExt<T> for $builder<T, R>
        where
            T: MigrationChain + Send + Sync + Clone + Debug + 'static,
        {
            fn with_envelope(self, carrier: EnvelopeCarrier) -> Self {
                let mut options = self.wire_options();
                options.carrier = carrier;
                self.with_wire_options(options)
                    .with_config(
                        aimdb_core::link_wire::ENVELOPE_KEY,
                        if carrier == EnvelopeCarrier::Inline {
                            "inline"
                        } else {
                            "bound"
                        },
                    )
                    .with_wire_finalizer(false, $finalize::<T>)
            }
            fn with_contract<U: SchemaType + ContractShape + VersionOf<T>>(self) -> Self {
                let mut options = self.wire_options();
                options.binding = Some(ContractRef::new(
                    crate::name_id(U::NAME),
                    U::VERSION,
                    U::SHAPE,
                ));
                self.with_wire_options(options)
                    .with_wire_finalizer(false, $finalize::<T>)
            }
        }
    };
}
envelope_ext!(InboundLinkBuilder, builtin_inbound);
envelope_ext!(OutboundLinkBuilder, builtin_outbound);
impl<T, R> VersionedInboundExt<T> for InboundLinkBuilder<T, R>
where
    T: MigrationChain + Send + Sync + Clone + Debug + 'static,
{
    fn accepts_from<const V: u32>(self) -> Self
    where
        T: Reaches<V>,
    {
        let mut options = self.wire_options();
        options.accepts_from = Some(V);
        self.with_wire_options(options)
            .with_wire_finalizer(false, builtin_inbound::<T>)
    }
}
impl<T, R> VersionedOutboundExt<T> for OutboundLinkBuilder<T, R>
where
    T: MigrationChain + Send + Sync + Clone + Debug + 'static,
{
    fn sends_as<const V: u32>(self) -> Self
    where
        T: Reaches<V>,
    {
        let mut options = self.wire_options();
        options.sends_as = Some(V);
        self.with_wire_options(options)
            .with_wire_finalizer(false, builtin_outbound::<T>)
    }
    fn without_shape(self) -> Self {
        let mut options = self.wire_options();
        options.include_shape = false;
        self.with_wire_options(options)
            .with_wire_finalizer(false, builtin_outbound::<T>)
    }
}
macro_rules! custom_ext {
    ($builder:ident, $finalize:ident, $builtin:ident) => {
        impl<T, R> VersionedCodecBuilderExt<T> for $builder<T, R>
        where
            T: MigrationChain + Send + Sync + Clone + Debug + 'static,
        {
            fn with_versioned_codec<C>(self, codec: C) -> Self
            where
                C: LinkCodec<T> + PayloadDecoder + PayloadEncoder,
            {
                let info = WireCodecInfo {
                    codec: C::WIRE_FORMAT.payload_codec(),
                    capacity: C::ENCODE_BUFFER_CAPACITY,
                    custom: true,
                };
                self.with_link_codec(codec.clone())
                    .with_wire_codec_info(Some(info))
                    .with_wire_finalizer(true, move |link| match link.wire_codec_info() {
                        Some(info) if info.custom => $finalize(link, codec.clone(), info),
                        _ => $builtin(link),
                    })
            }
        }
    };
}
custom_ext!(InboundLinkBuilder, inbound, builtin_inbound);
custom_ext!(OutboundLinkBuilder, outbound, builtin_outbound);
