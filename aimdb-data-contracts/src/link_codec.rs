//! Per-link codec selection for [`Linkable`](crate::Linkable) records.
//!
//! Codecs are selected while a link is registered and captured by AimDB's
//! existing typed serializer/deserializer closures. There is no runtime codec
//! registry or per-message lookup.
//!
//! ```rust,ignore
//! use aimdb_data_contracts::{
//!     link_codecs::{Cdr, Json, Postcard},
//!     LinkCodecBuilderExt, LinkCodecRegistrarExt,
//! };
//!
//! registrar.linked_to_with("serial://mcu/reading", Postcard::<128>);
//! registrar.linked_to_with("mqtt://cloud/reading", Json);
//! registrar.linked_to_with("zenoh://cell/reading", Cdr::<128>);
//! registrar
//!     .link_to("mqtt://cloud/alerts")
//!     .with_link_codec(Json)
//!     .with_config("qos", "1")
//!     .finish();
//! ```

use alloc::string::String;
#[cfg(any(
    feature = "linkable-json",
    feature = "linkable-postcard",
    feature = "linkable-cdr"
))]
use alloc::string::ToString;
use alloc::vec::Vec;
use core::fmt::Debug;

use aimdb_core::connector::{SerializeError, WIRE_FORMAT_KEY};
use aimdb_core::typed_api::{InboundConnectorBuilder, OutboundConnectorBuilder};

/// The encoding a [`Linkable`](crate::Linkable) or [`LinkCodec`] speaks.
///
/// Codec verbs record it in the link config under
/// [`WIRE_FORMAT_KEY`] so connectors can check it at build time.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireFormat {
    /// Not declared; nothing is recorded.
    Unspecified,
    /// JSON.
    Json,
    /// Postcard.
    Postcard,
    /// OMG CDR (XCDR1) with encapsulation header, as used by ROS 2.
    Cdr,
}

impl WireFormat {
    /// The value recorded under [`WIRE_FORMAT_KEY`]; `None` for `Unspecified`.
    pub const fn as_str(self) -> Option<&'static str> {
        match self {
            Self::Unspecified => None,
            Self::Json => Some("json"),
            Self::Postcard => Some("postcard"),
            Self::Cdr => Some("cdr"),
        }
    }

    /// Reads the format recorded in a link config; `Unspecified` if absent or
    /// unknown.
    pub fn recorded_in(config: &[(String, String)]) -> Self {
        let value = config
            .iter()
            .find(|(key, _)| key == WIRE_FORMAT_KEY)
            .map(|(_, value)| value.as_str());
        match value {
            Some("json") => Self::Json,
            Some("postcard") => Self::Postcard,
            Some("cdr") => Self::Cdr,
            _ => Self::Unspecified,
        }
    }
}

/// A wire codec selected for one inbound or outbound record link.
///
/// The owned [`encode`](Self::encode) operation is always required so an
/// outbound link can handle values larger than its preferred scratch buffer.
/// Set [`ENCODE_BUFFER_CAPACITY`](Self::ENCODE_BUFFER_CAPACITY) to `Some(n)`
/// only when [`encode_into`](Self::encode_into) has a bounded,
/// allocation-free success path. AimDB allocates that scratch storage once per
/// outbound route and reuses it serially.
///
/// Transport framing is deliberately outside this trait. [`decode`](Self::decode)
/// receives exactly one payload after the connector has applied its own frame
/// limits and partial-read handling.
pub trait LinkCodec<T>: Clone + Send + Sync + 'static {
    /// Preferred reusable outbound scratch capacity, in bytes.
    ///
    /// `None` keeps the route on owned serialization. `Some(n)` installs the
    /// scratch serializer alongside the mandatory owned fallback.
    const ENCODE_BUFFER_CAPACITY: Option<usize> = None;

    /// What this codec puts on the wire, recorded on each link it is installed on.
    const WIRE_FORMAT: WireFormat = WireFormat::Unspecified;

    /// Decode one connector payload into a record.
    fn decode(&self, bytes: &[u8]) -> Result<T, String>;

    /// Encode a record into owned bytes.
    fn encode(&self, value: &T) -> Result<Vec<u8>, SerializeError>;

    /// Encode a record into caller-owned storage.
    ///
    /// A codec that advertises a scratch capacity must not allocate on its
    /// steady-state success path. Return [`SerializeError::BufferTooSmall`] if
    /// the provided storage cannot hold the value; AimDB then invokes
    /// [`encode`](Self::encode) once for that value.
    fn encode_into(&self, value: &T, out: &mut [u8]) -> Result<usize, SerializeError>;
}

/// Built-in per-link codec markers.
pub mod link_codecs {
    /// Delegates to the record's existing [`Linkable`](crate::Linkable)
    /// implementation.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct Default;

    /// JSON via `serde_json`.
    ///
    /// JSON has no fixed encoded-size bound, so outbound links use the owned
    /// serializer rather than advertising a reusable scratch capacity.
    #[cfg(feature = "linkable-json")]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct Json;

    /// Postcard with a route-local reusable scratch capacity.
    ///
    /// Keep the number of distinct capacity values small on code-size-sensitive
    /// targets because each `N` is a separate monomorphization.
    #[cfg(feature = "linkable-postcard")]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct Postcard<const N: usize = 256>;

    /// Little-endian CDR (XCDR1) with its encapsulation header, as ROS 2
    /// sends it, with a route-local reusable scratch capacity.
    ///
    /// Decoding accepts both byte orders.
    #[cfg(feature = "linkable-cdr")]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct Cdr<const N: usize = 256>;
}

impl<T> LinkCodec<T> for link_codecs::Default
where
    T: crate::Linkable,
{
    const ENCODE_BUFFER_CAPACITY: Option<usize> = T::ENCODE_BUFFER_CAPACITY;
    const WIRE_FORMAT: WireFormat = T::WIRE_FORMAT;

    fn decode(&self, bytes: &[u8]) -> Result<T, String> {
        T::from_bytes(bytes)
    }

    fn encode(&self, value: &T) -> Result<Vec<u8>, SerializeError> {
        value.to_bytes().map_err(|_| SerializeError::InvalidData)
    }

    fn encode_into(&self, value: &T, out: &mut [u8]) -> Result<usize, SerializeError> {
        value.encode_into(out)
    }
}

#[cfg(feature = "linkable-json")]
impl<T> LinkCodec<T> for link_codecs::Json
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    const WIRE_FORMAT: WireFormat = WireFormat::Json;

    fn decode(&self, bytes: &[u8]) -> Result<T, String> {
        serde_json::from_slice(bytes).map_err(|error| error.to_string())
    }

    fn encode(&self, value: &T) -> Result<Vec<u8>, SerializeError> {
        serde_json::to_vec(value).map_err(|_| SerializeError::InvalidData)
    }

    fn encode_into(&self, value: &T, out: &mut [u8]) -> Result<usize, SerializeError> {
        let encoded = self.encode(value)?;
        let destination = out
            .get_mut(..encoded.len())
            .ok_or(SerializeError::BufferTooSmall)?;
        destination.copy_from_slice(&encoded);
        Ok(encoded.len())
    }
}

#[cfg(feature = "linkable-postcard")]
impl<T, const N: usize> LinkCodec<T> for link_codecs::Postcard<N>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    const ENCODE_BUFFER_CAPACITY: Option<usize> = Some(N);
    const WIRE_FORMAT: WireFormat = WireFormat::Postcard;

    fn decode(&self, bytes: &[u8]) -> Result<T, String> {
        postcard::from_bytes(bytes).map_err(|error| error.to_string())
    }

    fn encode(&self, value: &T) -> Result<Vec<u8>, SerializeError> {
        postcard::to_allocvec(value).map_err(|_| SerializeError::InvalidData)
    }

    fn encode_into(&self, value: &T, out: &mut [u8]) -> Result<usize, SerializeError> {
        match postcard::to_slice(value, out) {
            Ok(used) => Ok(used.len()),
            Err(postcard::Error::SerializeBufferFull) => Err(SerializeError::BufferTooSmall),
            Err(_) => Err(SerializeError::InvalidData),
        }
    }
}

#[cfg(feature = "linkable-cdr")]
impl<T, const N: usize> LinkCodec<T> for link_codecs::Cdr<N>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    const ENCODE_BUFFER_CAPACITY: Option<usize> = Some(N);
    const WIRE_FORMAT: WireFormat = WireFormat::Cdr;

    fn decode(&self, bytes: &[u8]) -> Result<T, String> {
        aimdb_cdr::from_bytes(bytes).map_err(|error| error.to_string())
    }

    fn encode(&self, value: &T) -> Result<Vec<u8>, SerializeError> {
        aimdb_cdr::to_vec(value).map_err(|_| SerializeError::InvalidData)
    }

    fn encode_into(&self, value: &T, out: &mut [u8]) -> Result<usize, SerializeError> {
        match aimdb_cdr::to_slice(value, out) {
            Ok(used) => Ok(used),
            Err(aimdb_cdr::Error::BufferTooSmall) => Err(SerializeError::BufferTooSmall),
            Err(_) => Err(SerializeError::InvalidData),
        }
    }
}

/// Selects a codec on an individual typed connector builder.
///
/// The builder type is preserved, so connector-specific extension methods can
/// be called before or after `with_link_codec`.
pub trait LinkCodecBuilderExt<T>: Sized
where
    T: Send + Sync + Clone + Debug + 'static,
{
    /// Install this codec on the current inbound or outbound link.
    ///
    /// Calling this method again replaces the complete codec strategy. In
    /// particular, an owned-only codec clears any scratch serializer installed
    /// by an earlier bounded codec. The codec's [`WireFormat`] is recorded
    /// under [`WIRE_FORMAT_KEY`].
    fn with_link_codec<C>(self, codec: C) -> Self
    where
        C: LinkCodec<T>;
}

impl<'r, 'a, T> LinkCodecBuilderExt<T> for OutboundConnectorBuilder<'r, 'a, T>
where
    T: Send + Sync + Clone + Debug + 'static,
{
    fn with_link_codec<C>(self, codec: C) -> Self
    where
        C: LinkCodec<T>,
    {
        let builder = match C::ENCODE_BUFFER_CAPACITY {
            Some(capacity) => {
                let scratch_codec = codec.clone();
                self.with_serializer(move |_ctx, value| codec.encode(value))
                    .with_serializer_into(capacity, move |_ctx, value, out| {
                        scratch_codec.encode_into(value, out)
                    })
            }
            None => self
                .with_serializer(move |_ctx, value| codec.encode(value))
                .clear_serializer_into(),
        };
        // After the setters, which clear any earlier record.
        match C::WIRE_FORMAT.as_str() {
            Some(format) => builder.with_config(WIRE_FORMAT_KEY, format),
            None => builder,
        }
    }
}

impl<'r, 'a, T> LinkCodecBuilderExt<T> for InboundConnectorBuilder<'r, 'a, T>
where
    T: Send + Sync + Clone + Debug + 'static,
{
    fn with_link_codec<C>(self, codec: C) -> Self
    where
        C: LinkCodec<T>,
    {
        let builder = self.with_deserializer(move |_ctx, bytes| codec.decode(bytes));
        match C::WIRE_FORMAT.as_str() {
            Some(format) => builder.with_config(WIRE_FORMAT_KEY, format),
            None => builder,
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(all(feature = "linkable-json", feature = "linkable-postcard"))]
    extern crate std;

    #[cfg(all(feature = "linkable-json", feature = "linkable-postcard"))]
    use alloc::boxed::Box;
    use alloc::string::{String, ToString};
    #[cfg(all(feature = "linkable-json", feature = "linkable-postcard"))]
    use alloc::sync::Arc;
    use alloc::vec;
    use alloc::vec::Vec;
    #[cfg(all(feature = "linkable-json", feature = "linkable-postcard"))]
    use core::future::Future;
    #[cfg(all(feature = "linkable-json", feature = "linkable-postcard"))]
    use core::pin::Pin;
    #[cfg(all(feature = "linkable-json", feature = "linkable-postcard"))]
    use core::task::{Context, Poll};

    #[cfg(all(feature = "linkable-json", feature = "linkable-postcard"))]
    use aimdb_core::buffer::{BufferReader, DynBuffer};
    #[cfg(all(feature = "linkable-json", feature = "linkable-postcard"))]
    use aimdb_core::connector::ConnectorBuilder;
    use aimdb_core::connector::SerializeError;
    #[cfg(all(feature = "linkable-json", feature = "linkable-postcard"))]
    use aimdb_core::executor::test_support::NoopRuntimeOps;
    #[cfg(all(feature = "linkable-json", feature = "linkable-postcard"))]
    use aimdb_core::{
        AimDb, AimDbBuilder, BoxFuture, DbError, DbResult, InboundDispatch, OutboundPayload,
        OutboundRoutes,
    };
    use serde::{Deserialize, Serialize};

    #[cfg(all(feature = "linkable-json", feature = "linkable-postcard"))]
    use super::LinkCodecBuilderExt;
    use super::{link_codecs, LinkCodec};
    #[cfg(all(feature = "linkable-json", feature = "linkable-postcard"))]
    use crate::LinkCodecRegistrarExt;
    use crate::{Linkable, SchemaType};

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct Reading {
        value: f32,
        sequence: u32,
    }

    impl SchemaType for Reading {
        const NAME: &'static str = "per_link_codec_reading";
    }

    impl Linkable for Reading {
        fn from_bytes(data: &[u8]) -> Result<Self, String> {
            serde_json::from_slice(data).map_err(|error| error.to_string())
        }

        fn to_bytes(&self) -> Result<Vec<u8>, String> {
            serde_json::to_vec(self).map_err(|error| error.to_string())
        }
    }

    #[test]
    fn default_codec_delegates_to_linkable() {
        let reading = Reading {
            value: 23.5,
            sequence: 7,
        };
        let codec = link_codecs::Default;
        let encoded = codec.encode(&reading).expect("default encode");

        assert_eq!(encoded, reading.to_bytes().expect("Linkable encode"));
        let decoded: Reading = codec.decode(&encoded).expect("default decode");
        assert_eq!(decoded, reading);

        let mut out = vec![0xA5; encoded.len() + 4];
        let written = codec
            .encode_into(&reading, &mut out)
            .expect("default encode_into");
        assert_eq!(&out[..written], encoded.as_slice());
        assert_eq!(&out[written..], &[0xA5; 4]);
    }

    #[derive(Clone, Copy)]
    struct OffsetCodec {
        offset: u32,
    }

    impl LinkCodec<Reading> for OffsetCodec {
        const ENCODE_BUFFER_CAPACITY: Option<usize> = Some(4);

        fn decode(&self, bytes: &[u8]) -> Result<Reading, String> {
            let encoded: [u8; 4] = bytes
                .try_into()
                .map_err(|_| "offset codec expects four bytes".to_string())?;
            Ok(Reading {
                value: 0.0,
                sequence: u32::from_le_bytes(encoded).wrapping_sub(self.offset),
            })
        }

        fn encode(&self, value: &Reading) -> Result<Vec<u8>, SerializeError> {
            Ok(value
                .sequence
                .wrapping_add(self.offset)
                .to_le_bytes()
                .to_vec())
        }

        fn encode_into(&self, value: &Reading, out: &mut [u8]) -> Result<usize, SerializeError> {
            let encoded = value.sequence.wrapping_add(self.offset).to_le_bytes();
            out.get_mut(..encoded.len())
                .ok_or(SerializeError::BufferTooSmall)?
                .copy_from_slice(&encoded);
            Ok(encoded.len())
        }
    }

    #[test]
    fn stateful_codec_configuration_is_used() {
        let reading = Reading {
            value: 0.0,
            sequence: 10,
        };
        let codec = OffsetCodec { offset: 32 };
        let mut out = [0_u8; 4];

        let written = codec
            .encode_into(&reading, &mut out)
            .expect("bounded encode");

        assert_eq!(written, 4);
        assert_eq!(u32::from_le_bytes(out), 42);
        assert_eq!(codec.decode(&out).expect("decode").sequence, 10);
    }

    #[cfg(feature = "linkable-json")]
    #[test]
    fn json_codec_round_trips_and_rejects_malformed_input() {
        let reading = Reading {
            value: 21.25,
            sequence: 8,
        };
        let codec = link_codecs::Json;
        let encoded = codec.encode(&reading).expect("JSON encode");

        let decoded: Reading = codec.decode(&encoded).expect("JSON decode");
        let malformed: Result<Reading, _> = codec.decode(b"{");
        assert_eq!(decoded, reading);
        assert!(malformed.is_err());
        assert_eq!(
            <link_codecs::Json as LinkCodec<Reading>>::ENCODE_BUFFER_CAPACITY,
            None
        );
    }

    #[cfg(feature = "linkable-postcard")]
    #[test]
    fn postcard_codec_handles_exact_and_undersized_buffers() {
        let reading = Reading {
            value: 19.75,
            sequence: 9,
        };
        let codec = link_codecs::Postcard::<64>;
        let owned = codec.encode(&reading).expect("Postcard encode");
        let mut exact = vec![0_u8; owned.len()];

        let written = codec
            .encode_into(&reading, &mut exact)
            .expect("exact Postcard buffer");

        assert_eq!(written, owned.len());
        assert_eq!(exact, owned);
        let decoded: Reading = codec.decode(&exact).expect("Postcard decode");
        assert_eq!(decoded, reading);

        let mut small = vec![0_u8; owned.len() - 1];
        assert_eq!(
            codec.encode_into(&reading, &mut small),
            Err(SerializeError::BufferTooSmall)
        );
        let malformed: Result<Reading, _> = codec.decode(&[]);
        assert!(malformed.is_err());
    }

    #[cfg(feature = "linkable-cdr")]
    #[test]
    fn cdr_codec_writes_header_and_handles_exact_and_undersized_buffers() {
        let reading = Reading {
            value: 1.5,
            sequence: 0x0102_0304,
        };
        let codec = link_codecs::Cdr::<64>;
        let owned = codec.encode(&reading).expect("CDR encode");
        // Header, then `f32` and `u32` little-endian; both 4-aligned.
        let mut expected = vec![0x00, 0x01, 0x00, 0x00];
        expected.extend_from_slice(&1.5_f32.to_le_bytes());
        expected.extend_from_slice(&0x0102_0304_u32.to_le_bytes());
        assert_eq!(owned, expected);

        let mut exact = vec![0_u8; owned.len()];
        let written = codec
            .encode_into(&reading, &mut exact)
            .expect("exact CDR buffer");
        assert_eq!(written, owned.len());
        assert_eq!(exact, owned);
        let decoded: Reading = codec.decode(&exact).expect("CDR decode");
        assert_eq!(decoded, reading);

        let mut small = vec![0_u8; owned.len() - 1];
        assert_eq!(
            codec.encode_into(&reading, &mut small),
            Err(SerializeError::BufferTooSmall)
        );
        let bad_header: Result<Reading, _> = codec.decode(&[0x00, 0x07, 0x00, 0x00]);
        assert!(bad_header.is_err());
        assert_eq!(
            <link_codecs::Cdr<64> as LinkCodec<Reading>>::ENCODE_BUFFER_CAPACITY,
            Some(64)
        );
    }

    #[cfg(feature = "linkable-cdr")]
    #[test]
    fn cdr_codec_decodes_big_endian() {
        let mut big_endian = vec![0x00, 0x00, 0x00, 0x00];
        big_endian.extend_from_slice(&1.5_f32.to_be_bytes());
        big_endian.extend_from_slice(&7_u32.to_be_bytes());

        let decoded: Reading = link_codecs::Cdr::<64>
            .decode(&big_endian)
            .expect("CDR_BE decode");
        assert_eq!(
            decoded,
            Reading {
                value: 1.5,
                sequence: 7
            }
        );
    }

    /// A fresh reader replays one value. Separate route subscriptions receive
    /// independent copies, matching a fan-out buffer's consumer semantics.
    #[cfg(all(feature = "linkable-json", feature = "linkable-postcard"))]
    struct CannedBuffer {
        value: Reading,
    }

    #[cfg(all(feature = "linkable-json", feature = "linkable-postcard"))]
    impl DynBuffer<Reading> for CannedBuffer {
        fn push(&self, _value: Reading) {}

        fn subscribe_boxed(&self) -> Box<dyn BufferReader<Reading> + Send> {
            Box::new(OneShotReader {
                value: Some(self.value.clone()),
            })
        }

        fn as_any(&self) -> &dyn core::any::Any {
            self
        }
    }

    #[cfg(all(feature = "linkable-json", feature = "linkable-postcard"))]
    struct OneShotReader {
        value: Option<Reading>,
    }

    #[cfg(all(feature = "linkable-json", feature = "linkable-postcard"))]
    impl OneShotReader {
        fn receive(&mut self) -> Result<Reading, DbError> {
            self.value.take().ok_or_else(|| DbError::BufferClosed {
                buffer_name: "per-link-codec-test".to_string(),
            })
        }
    }

    #[cfg(all(feature = "linkable-json", feature = "linkable-postcard"))]
    impl BufferReader<Reading> for OneShotReader {
        fn poll_recv(&mut self, _cx: &mut Context<'_>) -> Poll<Result<Reading, DbError>> {
            Poll::Ready(self.receive())
        }

        fn try_recv(&mut self) -> Result<Reading, DbError> {
            self.receive()
        }
    }

    #[cfg(all(feature = "linkable-json", feature = "linkable-postcard"))]
    struct CapturingBuffer {
        latest: Arc<std::sync::Mutex<Option<Reading>>>,
    }

    #[cfg(all(feature = "linkable-json", feature = "linkable-postcard"))]
    impl DynBuffer<Reading> for CapturingBuffer {
        fn push(&self, value: Reading) {
            *self.latest.lock().expect("capture lock poisoned") = Some(value);
        }

        fn subscribe_boxed(&self) -> Box<dyn BufferReader<Reading> + Send> {
            Box::new(OneShotReader { value: None })
        }

        fn as_any(&self) -> &dyn core::any::Any {
            self
        }
    }

    #[cfg(all(feature = "linkable-json", feature = "linkable-postcard"))]
    struct NoopConnector;

    #[cfg(all(feature = "linkable-json", feature = "linkable-postcard"))]
    impl ConnectorBuilder for NoopConnector {
        fn build<'a>(
            &'a self,
            _db: &'a AimDb,
        ) -> Pin<Box<dyn Future<Output = DbResult<Vec<BoxFuture>>> + Send + 'a>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn scheme(&self) -> &str {
            "test"
        }
    }

    #[cfg(all(feature = "linkable-json", feature = "linkable-postcard"))]
    #[test]
    fn one_record_type_uses_route_local_json_and_postcard_codecs() {
        futures::executor::block_on(async {
            let reading = Reading {
                value: 24.5,
                sequence: 11,
            };
            let json_in = Arc::new(std::sync::Mutex::new(None));
            let postcard_in = Arc::new(std::sync::Mutex::new(None));
            let mut builder = AimDbBuilder::new()
                .runtime(Arc::new(NoopRuntimeOps))
                .with_connector(NoopConnector);

            builder.configure::<Reading>("reading.out", |registrar| {
                registrar.buffer_raw(Box::new(CannedBuffer {
                    value: reading.clone(),
                }));
                registrar.linked_to_with("test://json", link_codecs::Json);
                registrar
                    .link_to("test://postcard")
                    .with_link_codec(link_codecs::Postcard::<64>)
                    .with_config("wire", "binary")
                    .finish();
                registrar
                    .linked_to_with("test://postcard-owned-fallback", link_codecs::Postcard::<1>);
                registrar
                    .link_to("test://postcard-replaced-by-json")
                    .with_link_codec(link_codecs::Postcard::<64>)
                    .with_link_codec(link_codecs::Json)
                    .finish();
            });

            let json_capture = json_in.clone();
            builder.configure::<Reading>("reading.json.in", move |registrar| {
                registrar.buffer_raw(Box::new(CapturingBuffer {
                    latest: json_capture,
                }));
                registrar.linked_from_with("test://json-in", link_codecs::Json);
            });

            let postcard_capture = postcard_in.clone();
            builder.configure::<Reading>("reading.postcard.in", move |registrar| {
                registrar.buffer_raw(Box::new(CapturingBuffer {
                    latest: postcard_capture,
                }));
                registrar
                    .link_from("test://postcard-in")
                    .with_link_codec(link_codecs::Postcard::<64>)
                    .with_config("wire", "binary")
                    .finish();
            });

            let (db, _runner) = builder.build().await.expect("codec routes build");
            let mut outbound = OutboundRoutes::new(&db, "test").expect("outbound routes");
            let route = |topic: &str| {
                outbound
                    .routes()
                    .iter()
                    .find(|route| &*route.default_topic == topic)
                    .expect("route")
                    .clone()
            };
            assert_eq!(outbound.routes().len(), 4);
            // A payload capacity of 0 means owned serialization only.
            assert_eq!(route("json").payload_capacity, 0);
            assert_eq!(route("postcard").payload_capacity, 64);
            assert!(route("postcard")
                .config
                .protocol_options
                .contains(&("wire".to_string(), "binary".to_string())));
            assert_eq!(route("postcard-owned-fallback").payload_capacity, 1);
            assert_eq!(
                route("postcard-replaced-by-json").payload_capacity,
                0,
                "owned-only replacement must clear the previous scratch codec"
            );

            // Each canned reader yields one value, then closes its route.
            let mut messages = std::collections::BTreeMap::new();
            while let Some(msg) = outbound.next().await {
                let owned = matches!(msg.payload, OutboundPayload::Owned(_));
                messages.insert(msg.topic.to_string(), (msg.payload.into_vec(), owned));
            }
            let message = |topic: &str| messages.get(topic).expect("message").clone();

            let (json_bytes, owned) = message("json");
            assert!(owned, "JSON must use owned serialization");
            let decoded_json: Reading = link_codecs::Json.decode(&json_bytes).expect("JSON decode");
            assert_eq!(decoded_json, reading);

            let (postcard_bytes, owned) = message("postcard");
            assert!(!owned, "Postcard must use route scratch storage");
            let decoded_postcard: Reading = link_codecs::Postcard::<64>
                .decode(&postcard_bytes)
                .expect("Postcard decode");
            assert_eq!(decoded_postcard, reading);
            assert_ne!(json_bytes, postcard_bytes);

            let (fallback_bytes, owned) = message("postcard-owned-fallback");
            assert!(owned, "undersized Postcard scratch must use owned fallback");
            let decoded_fallback: Reading = link_codecs::Postcard::<1>
                .decode(&fallback_bytes)
                .expect("fallback Postcard decode");
            assert_eq!(decoded_fallback, reading);

            let (replacement_bytes, owned) = message("postcard-replaced-by-json");
            assert!(owned, "replacement JSON codec must use owned serialization");
            assert_eq!(replacement_bytes, json_bytes);

            let inbound = InboundDispatch::new(&db, "test", &aimdb_core::ExactGrammar)
                .expect("inbound routes");
            assert_eq!(inbound.route_count(), 2);
            inbound.dispatch("json-in", &json_bytes);
            assert_eq!(
                json_in.lock().expect("JSON capture lock").as_ref(),
                Some(&reading)
            );

            inbound.dispatch("postcard-in", &postcard_bytes);
            assert_eq!(
                postcard_in.lock().expect("Postcard capture lock").as_ref(),
                Some(&reading)
            );
        });
    }

    #[test]
    fn wire_format_round_trips_through_link_config() {
        use super::WireFormat;
        use aimdb_core::connector::WIRE_FORMAT_KEY;

        for format in [WireFormat::Json, WireFormat::Postcard, WireFormat::Cdr] {
            let config = vec![(
                WIRE_FORMAT_KEY.to_string(),
                format.as_str().expect("named format").to_string(),
            )];
            assert_eq!(WireFormat::recorded_in(&config), format);
        }
        assert_eq!(WireFormat::Unspecified.as_str(), None);
        assert_eq!(WireFormat::recorded_in(&[]), WireFormat::Unspecified);
        let unknown = vec![(WIRE_FORMAT_KEY.to_string(), "xml".to_string())];
        assert_eq!(WireFormat::recorded_in(&unknown), WireFormat::Unspecified);
    }

    #[cfg(all(feature = "linkable-json", feature = "linkable-postcard"))]
    #[test]
    fn codec_verbs_record_the_last_codecs_wire_format() {
        use super::WireFormat;
        use crate::LinkableRegistrarExt;
        use aimdb_core::connector::WIRE_FORMAT_KEY;

        futures::executor::block_on(async {
            let reading = Reading {
                value: 1.0,
                sequence: 1,
            };
            let mut builder = AimDbBuilder::new()
                .runtime(Arc::new(NoopRuntimeOps))
                .with_connector(NoopConnector);
            builder.configure::<Reading>("reading.formats", |registrar| {
                registrar.buffer_raw(Box::new(CannedBuffer { value: reading }));
                registrar.linked_to_with("test://json", link_codecs::Json);
                registrar.linked_to_with("test://postcard", link_codecs::Postcard::<64>);
                // `Reading`'s own Linkable declares no format.
                registrar.linked_to("test://default");
                registrar
                    .link_to("test://replaced")
                    .with_link_codec(link_codecs::Postcard::<64>)
                    .with_link_codec(link_codecs::Json)
                    .finish();
                registrar
                    .link_to("test://custom-after-codec")
                    .with_link_codec(link_codecs::Json)
                    .with_serializer(|_ctx, _value| Ok(Vec::new()))
                    .finish();
            });
            builder.configure::<Reading>("reading.formats.in", |registrar| {
                registrar.buffer_raw(Box::new(CapturingBuffer {
                    latest: Arc::new(std::sync::Mutex::new(None)),
                }));
                registrar.linked_from_with("test://json-in", link_codecs::Json);
                registrar.linked_from("test://default-in");
            });
            let (db, _runner) = builder.build().await.expect("build");

            let format_of = |topic: &str, config: &[(String, String)]| {
                let records = config
                    .iter()
                    .filter(|(key, _)| key == WIRE_FORMAT_KEY)
                    .count();
                (topic.to_string(), WireFormat::recorded_in(config), records)
            };
            let outbound = OutboundRoutes::new(&db, "test").expect("outbound routes");
            let inbound = InboundDispatch::new(&db, "test", &aimdb_core::ExactGrammar)
                .expect("inbound routes");
            let mut formats: Vec<(String, WireFormat, usize)> = outbound
                .routes()
                .iter()
                .map(|route| format_of(&route.default_topic, &route.config.protocol_options))
                .chain(
                    inbound
                        .routes()
                        .iter()
                        .map(|route| format_of(&route.topic, &route.config.protocol_options)),
                )
                .collect();
            formats.sort_by(|a, b| a.0.cmp(&b.0));

            let expected = [
                ("custom-after-codec", WireFormat::Unspecified, 0),
                ("default", WireFormat::Unspecified, 0),
                ("default-in", WireFormat::Unspecified, 0),
                ("json", WireFormat::Json, 1),
                ("json-in", WireFormat::Json, 1),
                ("postcard", WireFormat::Postcard, 1),
                ("replaced", WireFormat::Json, 1),
            ];
            let actual: Vec<(&str, WireFormat, usize)> = formats
                .iter()
                .map(|(topic, format, n)| (topic.as_str(), *format, *n))
                .collect();
            assert_eq!(actual, expected);
        });
    }

    #[cfg(all(
        feature = "linkable-json",
        feature = "linkable-postcard",
        feature = "linkable-cdr"
    ))]
    #[test]
    fn cdr_verb_records_cdr_and_takes_the_bounded_path() {
        use super::WireFormat;

        futures::executor::block_on(async {
            let reading = Reading {
                value: 1.0,
                sequence: 1,
            };
            let mut builder = AimDbBuilder::new()
                .runtime(Arc::new(NoopRuntimeOps))
                .with_connector(NoopConnector);
            builder.configure::<Reading>("reading.cdr", |registrar| {
                registrar.buffer_raw(Box::new(CannedBuffer { value: reading }));
                registrar.linked_to_with("test://cdr", link_codecs::Cdr::<64>);
            });
            let (db, _runner) = builder.build().await.expect("build");

            let routes = OutboundRoutes::new(&db, "test").expect("outbound routes");
            let route = &routes.routes()[0];
            assert_eq!(
                WireFormat::recorded_in(&route.config.protocol_options),
                WireFormat::Cdr
            );
            assert_eq!(route.payload_capacity, 64);
        });
    }
}
