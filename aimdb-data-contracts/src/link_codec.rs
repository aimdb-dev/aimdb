//! Per-link codec selection for [`Linkable`](crate::Linkable) records.
//!
//! Codecs are selected while a link is registered and captured by AimDB's
//! existing typed serializer/deserializer closures. There is no runtime codec
//! registry or per-message lookup.
//!
//! ```rust,ignore
//! use aimdb_data_contracts::{
//!     link_codecs::{Json, Postcard},
//!     LinkCodecBuilderExt, LinkCodecRegistrarExt,
//! };
//!
//! registrar.linked_to_with("serial://mcu/reading", Postcard::<128>);
//! registrar.linked_to_with("mqtt://cloud/reading", Json);
//! registrar
//!     .link_to("mqtt://cloud/alerts")
//!     .with_link_codec(Json)
//!     .with_config("qos", "1")
//!     .finish();
//! ```

use alloc::string::String;
#[cfg(any(feature = "linkable-json", feature = "linkable-postcard"))]
use alloc::string::ToString;
use alloc::vec::Vec;
use core::fmt::Debug;

use aimdb_core::connector::SerializeError;
use aimdb_core::typed_api::{InboundConnectorBuilder, OutboundConnectorBuilder};

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
}

impl<T> LinkCodec<T> for link_codecs::Default
where
    T: crate::Linkable,
{
    const ENCODE_BUFFER_CAPACITY: Option<usize> = T::ENCODE_BUFFER_CAPACITY;

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
    /// by an earlier bounded codec.
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
        match C::ENCODE_BUFFER_CAPACITY {
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
        self.with_deserializer(move |_ctx, bytes| codec.decode(bytes))
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
    use aimdb_core::connector::SerializeError;
    #[cfg(all(feature = "linkable-json", feature = "linkable-postcard"))]
    use aimdb_core::connector::{ConnectorBuilder, SerializedPayload};
    #[cfg(all(feature = "linkable-json", feature = "linkable-postcard"))]
    use aimdb_core::executor::test_support::NoopRuntimeOps;
    #[cfg(all(feature = "linkable-json", feature = "linkable-postcard"))]
    use aimdb_core::{AimDb, AimDbBuilder, BoxFuture, DbError, DbResult};
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
            let routes = db.collect_outbound_routes("test");
            assert_eq!(routes.len(), 4);

            let json_route = routes
                .iter()
                .find(|route| route.topic == "json")
                .expect("JSON route");
            assert_eq!(json_route.source.serializer_scratch_capacity(), None);
            let mut json_reader = json_route.source.subscribe();
            let mut unused_scratch = [];
            let json_message = json_reader
                .recv_into(&db.runtime_ctx(), &mut unused_scratch)
                .await
                .expect("JSON payload");
            let SerializedPayload::Owned(json_bytes) = json_message.payload else {
                panic!("JSON must use owned serialization");
            };
            let decoded_json: Reading = link_codecs::Json.decode(&json_bytes).expect("JSON decode");
            assert_eq!(decoded_json, reading);

            let postcard_route = routes
                .iter()
                .find(|route| route.topic == "postcard")
                .expect("Postcard route");
            assert_eq!(
                postcard_route.source.serializer_scratch_capacity(),
                Some(64)
            );
            assert!(postcard_route
                .config
                .contains(&("wire".to_string(), "binary".to_string())));
            let mut postcard_reader = postcard_route.source.subscribe();
            let mut scratch = [0_u8; 64];
            let postcard_message = postcard_reader
                .recv_into(&db.runtime_ctx(), &mut scratch)
                .await
                .expect("Postcard payload");
            let SerializedPayload::Scratch { len } = postcard_message.payload else {
                panic!("Postcard must use route scratch storage");
            };
            let decoded_postcard: Reading = link_codecs::Postcard::<64>
                .decode(&scratch[..len])
                .expect("Postcard decode");
            assert_eq!(decoded_postcard, reading);
            assert_ne!(json_bytes, scratch[..len]);

            let fallback_route = routes
                .iter()
                .find(|route| route.topic == "postcard-owned-fallback")
                .expect("undersized Postcard route");
            assert_eq!(fallback_route.source.serializer_scratch_capacity(), Some(1));
            let mut fallback_reader = fallback_route.source.subscribe();
            let mut undersized_scratch = [0_u8; 1];
            let fallback_message = fallback_reader
                .recv_into(&db.runtime_ctx(), &mut undersized_scratch)
                .await
                .expect("owned Postcard fallback");
            let SerializedPayload::Owned(fallback_bytes) = fallback_message.payload else {
                panic!("undersized Postcard scratch must use owned fallback");
            };
            let decoded_fallback: Reading = link_codecs::Postcard::<1>
                .decode(&fallback_bytes)
                .expect("fallback Postcard decode");
            assert_eq!(decoded_fallback, reading);

            let replacement_route = routes
                .iter()
                .find(|route| route.topic == "postcard-replaced-by-json")
                .expect("replacement route");
            assert_eq!(
                replacement_route.source.serializer_scratch_capacity(),
                None,
                "owned-only replacement must clear the previous scratch codec"
            );
            let mut replacement_reader = replacement_route.source.subscribe();
            let replacement_message = replacement_reader
                .recv_into(&db.runtime_ctx(), &mut [])
                .await
                .expect("replacement JSON payload");
            let SerializedPayload::Owned(replacement_bytes) = replacement_message.payload else {
                panic!("replacement JSON codec must use owned serialization");
            };
            assert_eq!(replacement_bytes, json_bytes);

            let inbound = db.collect_inbound_routes("test");
            assert_eq!(inbound.len(), 2);
            let json_ingest = inbound
                .iter()
                .find(|(topic, _)| topic == "json-in")
                .map(|(_, ingest)| ingest)
                .expect("JSON ingest route");
            json_ingest(&db.runtime_ctx(), &json_bytes).expect("JSON ingest");
            assert_eq!(
                json_in.lock().expect("JSON capture lock").as_ref(),
                Some(&reading)
            );

            let postcard_ingest = inbound
                .iter()
                .find(|(topic, _)| topic == "postcard-in")
                .map(|(_, ingest)| ingest)
                .expect("Postcard ingest route");
            postcard_ingest(&db.runtime_ctx(), &scratch[..len]).expect("Postcard ingest");
            assert_eq!(
                postcard_in.lock().expect("Postcard capture lock").as_ref(),
                Some(&reading)
            );
        });
    }
}
