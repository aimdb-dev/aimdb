//! One admission procedure for inline and statically bound values (design 062 §7).
//!
//! Successful admission returns core's `IngestValue`. The fused core ingest
//! callback produces it, stamps it on the local instance clock, and then counts
//! its outcome. Admission itself neither reads a clock nor trusts a wire stamp.

use alloc::{
    borrow::Cow,
    string::{String, ToString},
};
use core::{cell::Cell, marker::PhantomData};

use aimdb_core::envelope::{ContractRef, Envelope, EnvelopeError, PayloadCodec};
use aimdb_core::{AdmissionContext, AdmissionDetail, IngestError, IngestValue, Rejection};

use crate::{ContractShape, MigrationChain, PayloadDecoder, Reaches, SchemaType, VersionOf};

/// An accepted value with upgrade metadata, or a countable rejection.
/// Core stamps and counts successful values only when they are produced.
pub type Admit<T> = Result<IngestValue<T>, IngestError>;

/// Registration-time contract, accepted versions, carrier, and decoder for a link.
///
/// `T` fixes the current contract; `D` stays concrete, so decoding historical
/// types needs neither a codec registry nor type erasure. The supplied codec ID
/// must describe `D`. Link builders derive it from their selected `LinkCodec`.
pub struct LinkContract<T, D> {
    current: ContractRef,
    accepted_min: u32,
    accepted_max: u32,
    binding: Option<ContractRef>,
    codec: PayloadCodec,
    decoder: D,
    _current: PhantomData<fn() -> T>,
}

impl<T: MigrationChain, D: PayloadDecoder> LinkContract<T, D> {
    /// An inline link accepting only the current version.
    pub fn inline(codec: PayloadCodec, decoder: D) -> Self {
        Self {
            current: ContractRef::new(crate::name_id(T::NAME), T::VERSION, T::shape_of(T::VERSION)),
            accepted_min: T::VERSION,
            accepted_max: T::VERSION,
            binding: None,
            codec,
            decoder,
            _current: PhantomData,
        }
    }

    /// An inline link accepting `V..=T::VERSION`, proven by the chain.
    pub fn inline_from<const V: u32>(codec: PayloadCodec, decoder: D) -> Self
    where
        T: Reaches<V>,
    {
        Self {
            accepted_min: V,
            ..Self::inline(codec, decoder)
        }
    }

    /// The default bare link, statically bound to the current version.
    pub fn bound_current(codec: PayloadCodec, decoder: D) -> Self {
        let mut link = Self::inline(codec, decoder);
        link.binding = Some(link.current);
        link
    }

    /// A bare link carrying exactly version `U`, with a compile-time family proof.
    /// This binding replaces the inline accepted range; the two are not combined.
    pub fn bound<U>(codec: PayloadCodec, decoder: D) -> Self
    where
        U: SchemaType + ContractShape + VersionOf<T>,
    {
        let mut link = Self::inline(codec, decoder);
        link.accepted_min = U::VERSION;
        link.accepted_max = U::VERSION;
        link.binding = Some(ContractRef::new(
            crate::name_id(U::NAME),
            U::VERSION,
            U::SHAPE,
        ));
        link
    }

    pub(crate) fn configured(
        codec: PayloadCodec,
        decoder: D,
        binding: Option<ContractRef>,
        minimum: Option<u32>,
    ) -> Self {
        let mut policy = Self::inline(codec, decoder);
        if let Some(binding) = binding {
            policy.accepted_min = binding.version();
            policy.accepted_max = binding.version();
            policy.binding = Some(binding);
        } else if let Some(minimum) = minimum {
            policy.accepted_min = minimum;
        }
        policy
    }

    pub fn contract(&self) -> ContractRef {
        self.current
    }
    pub fn accepted_min(&self) -> u32 {
        self.accepted_min
    }
    pub fn accepted_max(&self) -> u32 {
        self.accepted_max
    }
    pub fn codec(&self) -> PayloadCodec {
        self.codec
    }
    pub fn is_bound(&self) -> bool {
        self.binding.is_some()
    }

    /// Interpret bytes according to the configured carrier, never by sniffing.
    pub fn envelope<'a>(&self, frame: &'a [u8]) -> Result<Envelope<'a>, IngestError> {
        match self.binding {
            Some(binding) => Ok(Envelope::bound(binding, self.codec, frame)),
            None => Envelope::parse(frame).map_err(|error| {
                let reason = match error {
                    EnvelopeError::UnsupportedFormat => Rejection::UnsupportedFormat,
                    _ => Rejection::Malformed,
                };
                IngestError::rejected(
                    reason,
                    self.context(None, AdmissionDetail::Header(error)),
                    "",
                )
            }),
        }
    }

    fn context(&self, received: Option<ContractRef>, detail: AdmissionDetail) -> AdmissionContext {
        AdmissionContext {
            received,
            accepted_name: T::NAME,
            accepted_name_id: self.current.name_id(),
            accepted_min: self.accepted_min,
            accepted_max: self.accepted_max,
            detail,
        }
    }

    /// Carries what was received and accepted as data: the text is formatted
    /// only when the error is displayed, so a rejection with a static or
    /// empty `message` does not allocate.
    fn reject(
        &self,
        env: &Envelope<'_>,
        reason: Rejection,
        detail: AdmissionDetail,
        message: impl Into<Cow<'static, str>>,
    ) -> IngestError {
        IngestError::rejected(reason, self.context(Some(env.contract()), detail), message)
    }
}

/// Parse or bind the configured carrier, then use the same admission procedure.
/// Suitable for core's `with_admitting_deserializer` callback.
pub fn admit_frame<T: MigrationChain, D: PayloadDecoder>(
    frame: &[u8],
    link: &LinkContract<T, D>,
) -> Admit<T> {
    admit(&link.envelope(frame)?, link)
}

/// Check identity, versions, historical shape, and codec before decoding once.
///
/// Rejection precedence follows design 062 §7. Missing locked shapes skip the
/// shape check. Codec `Unspecified` uses the link's decoder. On bound and
/// inline links alike the decoder must consume the entire payload (one frame,
/// one value); a remainder is a `decode_error`. Decoder or conversion failures count as `decode_error`. No route statistics
/// are mutated here: the router counts the result after core's fused ingestion.
pub fn admit<T: MigrationChain, D: PayloadDecoder>(
    env: &Envelope<'_>,
    link: &LinkContract<T, D>,
) -> Admit<T> {
    let received = env.contract();
    if received.name_id() != link.current.name_id() {
        return Err(link.reject(env, Rejection::ContractMismatch, AdmissionDetail::Name, ""));
    }
    if received.version() > T::VERSION || received.version() > link.accepted_max {
        return Err(link.reject(
            env,
            Rejection::TooNew,
            AdmissionDetail::Message,
            "downgrade on ingest is not permitted",
        ));
    }
    if received.version() < link.accepted_min {
        return Err(link.reject(
            env,
            Rejection::TooOld,
            AdmissionDetail::Message,
            "version below the link's accepted minimum",
        ));
    }
    if let (Some(actual), Some(expected)) = (received.shape(), T::shape_of(received.version())) {
        if actual != expected {
            return Err(link.reject(
                env,
                Rejection::ShapeMismatch,
                AdmissionDetail::Shape {
                    received: actual,
                    expected,
                },
                "",
            ));
        }
    }
    if env.codec() != PayloadCodec::Unspecified && env.codec() != link.codec {
        return Err(link.reject(
            env,
            Rejection::CodecMismatch,
            AdmissionDetail::Codec {
                received: env.codec(),
                expected: link.codec,
            },
            "",
        ));
    }

    // The migration API returns T, so capture decoder consumption without changing
    // that API or parsing twice. State is stack-local even for concurrent routes.
    let decoder = TrackedDecoder {
        inner: &link.decoder,
        used: Cell::new(None),
        attempted: Cell::new(false),
    };
    let value = if received.version() == T::VERSION {
        decoder
            .decode_payload::<T>(env.payload())
            .map(|(value, _)| value)
            .map_err(|error| decode_error(link, env, error))?
    } else {
        T::migrate_from(received.version(), env.payload(), &decoder)
            .map_err(|error| decode_error(link, env, error.to_string()))?
    };
    let Some(used) = decoder.used.get() else {
        return Err(link.reject(
            env,
            Rejection::DecodeError,
            AdmissionDetail::Message,
            "migration did not decode its payload",
        ));
    };
    if let Err(detail) = crate::payload_codec::check_consumed(used, env.payload().len()) {
        return Err(decode_error(link, env, detail));
    }
    if received.version() == T::VERSION {
        Ok(IngestValue::current(value))
    } else {
        Ok(IngestValue::upgraded(value, received.version()))
    }
}

/// A decoder's own message, moved into the error without reformatting.
fn decode_error<T: MigrationChain, D: PayloadDecoder>(
    link: &LinkContract<T, D>,
    env: &Envelope<'_>,
    message: String,
) -> IngestError {
    link.reject(
        env,
        Rejection::DecodeError,
        AdmissionDetail::Message,
        message,
    )
}

struct TrackedDecoder<'a, D> {
    inner: &'a D,
    used: Cell<Option<usize>>,
    attempted: Cell<bool>,
}
impl<D: PayloadDecoder> PayloadDecoder for TrackedDecoder<'_, D> {
    fn decode_payload<T: serde::de::DeserializeOwned>(
        &self,
        bytes: &[u8],
    ) -> Result<(T, usize), String> {
        if self.attempted.replace(true) {
            return Err(String::from("migration attempted more than one decode"));
        }
        let (value, used) = self.inner.decode_payload(bytes)?;
        if used > bytes.len() {
            return Err(String::from("decoder reported an invalid consumed length"));
        }
        self.used.set(Some(used));
        Ok((value, used))
    }
}
