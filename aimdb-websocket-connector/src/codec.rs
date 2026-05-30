//! Per-connection WS-JSON [`EnvelopeCodec`] (Phase 4 — doc 039 § 2).
//!
//! Maps the WS wire ([`ClientMessage`]/[`ServerMessage`]) onto the engine's
//! [`Inbound`]/[`Outbound`] so `run_session` ([`aimdb_core::session::run_session`])
//! can drive a WebSocket exactly as it drives AimX.
//!
//! **Why per-connection (not the shared `Arc<C>` that `serve` uses).** `decode`
//! is **1→1**, so it cannot fan a multi-topic `Subscribe` into N
//! `Inbound::Subscribe` — the transport splits the frame instead (see
//! [`crate::transport`]) and the codec synthesizes a `u64` id per topic, which the
//! `Subscribed` ack and `Unsubscribe` map back to a topic. That `id↔topic`
//! bookkeeping is per-connection state a shared `Arc<C>` cannot hold. Option A
//! calls `run_session(conn, &codec, …)` directly (only `serve` shares `Arc<C>`),
//! so each upgrade builds its own `WsCodec` holding the maps behind a `Mutex` (it
//! stays `Send + Sync`; encode/decode take `&self`).
//!
//! **Data frames are pre-serialized by the bus.** The hot fan-out path does *not*
//! pass through the id maps: [`ClientManager::broadcast`](crate::client_manager)
//! serializes the complete `Data` frame **once** (it owns the real topic) and the
//! codec writes it verbatim — O(1) in subscribers. The explicit `Subscribed` ack
//! and late-join `Snapshot` are engine emissions (`Outbound::Subscribed` +
//! `Session::snapshot`, gated by `SessionConfig::acks_subscribe`); the codec maps
//! those to wire frames.

use std::collections::HashMap;
use std::sync::Mutex;

use aimdb_core::{CodecError, Inbound, Outbound, Payload, RpcError};
use serde_json::Value;

use crate::protocol::{ClientMessage, ErrorCode, ServerMessage};

/// Per-connection id bookkeeping (doc 039 § 2). Lives behind a `Mutex` so the
/// `&self` codec methods can mutate it.
#[derive(Default)]
struct WsCodecState {
    /// Monotonic id allocator for engine `Subscribe`/`Request` correlation. The
    /// WS client never sees these ids — they exist only for the engine's demux.
    next_id: u64,
    /// WS topic → engine `Subscribe.id` (synthesized on subscribe; consulted by
    /// `Unsubscribe`).
    topic_to_id: HashMap<String, u64>,
    /// Engine `sub` id → wire topic (consulted when encoding `Event`→`Data` and
    /// the `Subscribed` ack).
    id_to_topic: HashMap<u64, String>,
}

impl WsCodecState {
    /// Allocate (or reuse) the engine subscribe id for `topic`.
    fn alloc_sub(&mut self, topic: &str) -> u64 {
        if let Some(id) = self.topic_to_id.get(topic) {
            return *id;
        }
        self.next_id += 1;
        let id = self.next_id;
        self.topic_to_id.insert(topic.to_string(), id);
        self.id_to_topic.insert(id, topic.to_string());
        id
    }

    /// Allocate a bare correlation id (for `Query`/`ListTopics` requests).
    fn alloc_req(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Drop the mapping for `topic`, returning its engine id if known.
    fn remove_topic(&mut self, topic: &str) -> Option<u64> {
        let id = self.topic_to_id.remove(topic)?;
        self.id_to_topic.remove(&id);
        Some(id)
    }

    /// Resolve an engine `sub` id (as the engine's `&str` form) back to its topic.
    fn topic_of(&self, sub: &str) -> Option<String> {
        let id: u64 = sub.parse().ok()?;
        self.id_to_topic.get(&id).cloned()
    }
}

/// A per-connection WS-JSON codec. Construct one per accepted connection (server)
/// or per dialed connection (client).
pub struct WsCodec {
    state: Mutex<WsCodecState>,
}

impl WsCodec {
    /// Build a fresh codec with empty id maps.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(WsCodecState::default()),
        }
    }
}

impl Default for WsCodec {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse record-value bytes as JSON, falling back to a JSON string (mirrors the
/// legacy `ClientManager` behavior so the wire is byte-identical).
pub(crate) fn parse_payload(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(bytes).into_owned()))
}

/// Map an engine [`RpcError`] to a wire [`ErrorCode`].
fn rpc_to_code(err: &RpcError) -> ErrorCode {
    match err {
        RpcError::NotFound => ErrorCode::UnknownTopic,
        RpcError::Denied => ErrorCode::Forbidden,
        _ => ErrorCode::ServerError,
    }
}

/// Serialize a [`ServerMessage`] into `out` as one WS-JSON text frame.
fn write_server(out: &mut Vec<u8>, msg: &ServerMessage) -> Result<(), CodecError> {
    let bytes = serde_json::to_vec(msg).map_err(|_| CodecError::Malformed)?;
    out.extend_from_slice(&bytes);
    Ok(())
}

impl aimdb_core::EnvelopeCodec for WsCodec {
    // ---- server direction: read a ClientMessage, write a ServerMessage ------

    fn decode(&self, frame: &[u8]) -> Result<Inbound, CodecError> {
        let msg: ClientMessage =
            serde_json::from_slice(frame).map_err(|_| CodecError::Malformed)?;
        let mut st = self.state.lock().unwrap();
        match msg {
            // The transport splits multi-topic frames, so exactly one topic here.
            ClientMessage::Subscribe { topics } => {
                let topic = topics.into_iter().next().ok_or(CodecError::Malformed)?;
                let id = st.alloc_sub(&topic);
                Ok(Inbound::Subscribe { id, topic })
            }
            ClientMessage::Unsubscribe { topics } => {
                let topic = topics.into_iter().next().ok_or(CodecError::Malformed)?;
                let id = st.remove_topic(&topic).ok_or(CodecError::Malformed)?;
                Ok(Inbound::Unsubscribe {
                    sub: id.to_string(),
                })
            }
            ClientMessage::Write { topic, payload } => {
                let bytes = serde_json::to_vec(&payload).map_err(|_| CodecError::Malformed)?;
                Ok(Inbound::Write {
                    topic,
                    payload: Payload::from(bytes.as_slice()),
                })
            }
            ClientMessage::Ping => Ok(Inbound::Ping),
            // The whole frame (incl. the WS `String` correlation id) rides as
            // `params`; the dispatch parses it and the id round-trips in the
            // response, so no per-request id map is needed.
            ClientMessage::Query { .. } => Ok(Inbound::Request {
                id: st.alloc_req(),
                method: "query".to_string(),
                params: Payload::from(frame),
            }),
            ClientMessage::ListTopics { .. } => Ok(Inbound::Request {
                id: st.alloc_req(),
                method: "list_topics".to_string(),
                params: Payload::from(frame),
            }),
        }
    }

    fn encode(&self, msg: Outbound<'_>, out: &mut Vec<u8>) -> Result<(), CodecError> {
        match msg {
            // The bus pre-serializes the complete `Data` frame **once** per
            // broadcast (it knows the real topic + raw-payload mode), so fan-out
            // is O(1) in subscribers, not O(N) re-serializations — see
            // `ClientManager::broadcast`. The codec just writes it verbatim.
            Outbound::Event { data, .. } => {
                out.extend_from_slice(&data);
                Ok(())
            }
            Outbound::Snapshot { topic, data } => write_server(
                out,
                &ServerMessage::Snapshot {
                    topic: topic.to_string(),
                    payload: Some(parse_payload(&data)),
                },
            ),
            Outbound::Subscribed { sub } => {
                let topic = self
                    .state
                    .lock()
                    .unwrap()
                    .topic_of(sub)
                    .ok_or(CodecError::Malformed)?;
                write_server(
                    out,
                    &ServerMessage::Subscribed {
                        topics: vec![topic],
                    },
                )
            }
            Outbound::Pong => write_server(out, &ServerMessage::Pong),
            // `Reply::Ok` payloads are already a complete `ServerMessage` JSON
            // (`QueryResult`/`TopicList`/`Error`) built by the dispatch with the
            // client's `String` id spliced in — write them verbatim.
            Outbound::Reply { result, .. } => match result {
                Ok(payload) => {
                    out.extend_from_slice(&payload);
                    Ok(())
                }
                Err(e) => write_server(
                    out,
                    &ServerMessage::Error {
                        code: rpc_to_code(&e),
                        topic: None,
                        message: format!("{e:?}"),
                    },
                ),
            },
        }
    }

    // ---- client direction: write a ClientMessage, read a ServerMessage ------
    // Used by the WS client port (`run_client`, Workstream D). The client engine
    // is configured topic-routed, so `Event.sub` carries the topic.

    fn encode_inbound(&self, msg: Inbound, out: &mut Vec<u8>) -> Result<(), CodecError> {
        let client_msg = match msg {
            Inbound::Subscribe { id, topic } => {
                let mut st = self.state.lock().unwrap();
                st.topic_to_id.insert(topic.clone(), id);
                st.id_to_topic.insert(id, topic.clone());
                ClientMessage::Subscribe {
                    topics: vec![topic],
                }
            }
            Inbound::Unsubscribe { sub } => {
                let topic = {
                    let mut st = self.state.lock().unwrap();
                    st.topic_of(&sub)
                        .inspect(|t| {
                            st.remove_topic(t);
                        })
                        .ok_or(CodecError::Malformed)?
                };
                ClientMessage::Unsubscribe {
                    topics: vec![topic],
                }
            }
            Inbound::Write { topic, payload } => ClientMessage::Write {
                topic,
                payload: parse_payload(&payload),
            },
            Inbound::Ping => ClientMessage::Ping,
            // `params` is already a complete `ClientMessage` JSON (`Query`/
            // `ListTopics`) — write it verbatim.
            Inbound::Request { params, .. } => {
                out.extend_from_slice(&params);
                return Ok(());
            }
        };
        let bytes = serde_json::to_vec(&client_msg).map_err(|_| CodecError::Malformed)?;
        out.extend_from_slice(&bytes);
        Ok(())
    }

    fn decode_outbound<'a>(&self, frame: &'a [u8]) -> Result<Outbound<'a>, CodecError> {
        // `Outbound::Event`/`Snapshot` borrow `topic` as `&'a str` from the frame.
        // serde's internally-tagged enums can't borrow (they buffer), so we peek
        // the `type` tag, then deserialize the matching struct that borrows the
        // topic slice **zero-copy** (no interning, no leak). Topics are record
        // keys without JSON escapes, so the borrow always succeeds.
        let tag: TagOnly = serde_json::from_slice(frame).map_err(|_| CodecError::Malformed)?;
        match tag.ty {
            "data" => {
                let d: TopicValueRef =
                    serde_json::from_slice(frame).map_err(|_| CodecError::Malformed)?;
                Ok(Outbound::Event {
                    sub: d.topic,
                    seq: 0,
                    data: value_to_payload(d.payload),
                })
            }
            "snapshot" => {
                let d: TopicValueRef =
                    serde_json::from_slice(frame).map_err(|_| CodecError::Malformed)?;
                Ok(Outbound::Snapshot {
                    topic: d.topic,
                    data: value_to_payload(d.payload),
                })
            }
            // Informational on the client; the engine ignores `Subscribed`, so the
            // `sub` value is irrelevant — hand back an empty borrow (no leak).
            "subscribed" => Ok(Outbound::Subscribed { sub: "" }),
            "pong" => Ok(Outbound::Pong),
            // Query/list responses + errors are RPC replies; the caller-RPC path
            // is not wired on the WS client (records mirror via Data/Snapshot),
            // so map them to a benign Pong.
            _ => Ok(Outbound::Pong),
        }
    }
}

/// Zero-copy peek at the `"type"` discriminant (the tag is short ASCII — no escapes).
#[derive(serde::Deserialize)]
struct TagOnly<'a> {
    #[serde(rename = "type", borrow)]
    ty: &'a str,
}

/// Zero-copy view of a `Data`/`Snapshot` frame: the `topic` borrows the frame.
#[derive(serde::Deserialize)]
struct TopicValueRef<'a> {
    #[serde(borrow)]
    topic: &'a str,
    payload: Option<Value>,
}

/// Serialize a WS payload `Value` back to record-value bytes.
fn value_to_payload(payload: Option<Value>) -> Payload {
    let bytes = payload
        .as_ref()
        .and_then(|v| serde_json::to_vec(v).ok())
        .unwrap_or_default();
    Payload::from(bytes.as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aimdb_core::EnvelopeCodec;

    fn sub(codec: &WsCodec, topic: &str) -> u64 {
        let frame = serde_json::to_vec(&ClientMessage::Subscribe {
            topics: vec![topic.to_string()],
        })
        .unwrap();
        match codec.decode(&frame).unwrap() {
            Inbound::Subscribe { id, topic: t } => {
                assert_eq!(t, topic);
                id
            }
            _ => panic!("expected Subscribe"),
        }
    }

    #[test]
    fn event_is_written_verbatim() {
        // The bus pre-serializes the complete Data frame; encode passes it through
        // (O(1) fan-out). `sub`/`seq` are irrelevant on this path.
        let codec = WsCodec::new();
        let frame = serde_json::to_vec(&ServerMessage::Data {
            topic: "sensors/temp/vienna".into(),
            payload: Some(serde_json::json!(22.5)),
            ts: 123,
        })
        .unwrap();
        let mut out = Vec::new();
        codec
            .encode(
                Outbound::Event {
                    sub: "ignored",
                    seq: 7,
                    data: Payload::from(frame.as_slice()),
                },
                &mut out,
            )
            .unwrap();
        assert_eq!(out, frame);
    }

    #[test]
    fn decode_outbound_borrows_topic_without_leaking() {
        // Client direction: a Data frame decodes to a topic-routed Event whose
        // `sub` is the topic, borrowed zero-copy from the frame.
        let codec = WsCodec::new();
        let frame = serde_json::to_vec(&ServerMessage::Data {
            topic: "weather/vienna".into(),
            payload: Some(serde_json::json!("sunny")),
            ts: 0,
        })
        .unwrap();
        match codec.decode_outbound(&frame).unwrap() {
            Outbound::Event { sub, data, .. } => {
                assert_eq!(sub, "weather/vienna");
                assert_eq!(&data[..], b"\"sunny\"");
            }
            _ => panic!("expected Event"),
        }
    }

    #[test]
    fn subscribed_ack_maps_to_topic() {
        let codec = WsCodec::new();
        let id = sub(&codec, "a/b");
        let mut out = Vec::new();
        codec
            .encode(
                Outbound::Subscribed {
                    sub: &id.to_string(),
                },
                &mut out,
            )
            .unwrap();
        let v: ServerMessage = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            v,
            ServerMessage::Subscribed {
                topics: vec!["a/b".into()]
            }
        );
    }

    #[test]
    fn unsubscribe_resolves_topic_to_id() {
        let codec = WsCodec::new();
        let id = sub(&codec, "x/y");
        let frame = serde_json::to_vec(&ClientMessage::Unsubscribe {
            topics: vec!["x/y".to_string()],
        })
        .unwrap();
        match codec.decode(&frame).unwrap() {
            Inbound::Unsubscribe { sub } => assert_eq!(sub, id.to_string()),
            _ => panic!("expected Unsubscribe"),
        }
    }

    #[test]
    fn write_carries_payload() {
        let codec = WsCodec::new();
        let frame = serde_json::to_vec(&ClientMessage::Write {
            topic: "cmd/set".to_string(),
            payload: serde_json::json!({"v": 1}),
        })
        .unwrap();
        match codec.decode(&frame).unwrap() {
            Inbound::Write { topic, payload } => {
                assert_eq!(topic, "cmd/set");
                let v: Value = serde_json::from_slice(&payload).unwrap();
                assert_eq!(v, serde_json::json!({"v": 1}));
            }
            _ => panic!("expected Write"),
        }
    }

    #[test]
    fn query_passes_frame_as_params_and_reply_writes_verbatim() {
        let codec = WsCodec::new();
        let frame = serde_json::to_vec(&ClientMessage::Query {
            id: "q1".to_string(),
            pattern: "#".to_string(),
            from: None,
            to: None,
            limit: None,
        })
        .unwrap();
        match codec.decode(&frame).unwrap() {
            Inbound::Request { method, params, .. } => {
                assert_eq!(method, "query");
                assert_eq!(&params[..], &frame[..]);
            }
            _ => panic!("expected Request"),
        }

        // A dispatch reply (already a full ServerMessage) is written verbatim.
        let reply = serde_json::to_vec(&ServerMessage::QueryResult {
            id: "q1".to_string(),
            records: vec![],
            total: 0,
        })
        .unwrap();
        let mut out = Vec::new();
        codec
            .encode(
                Outbound::Reply {
                    id: 7,
                    result: Ok(Payload::from(reply.as_slice())),
                },
                &mut out,
            )
            .unwrap();
        let v: ServerMessage = serde_json::from_slice(&out).unwrap();
        assert!(matches!(v, ServerMessage::QueryResult { id, .. } if id == "q1"));
    }

    #[test]
    fn ping_pong() {
        let codec = WsCodec::new();
        let frame = serde_json::to_vec(&ClientMessage::Ping).unwrap();
        assert!(matches!(codec.decode(&frame).unwrap(), Inbound::Ping));
        let mut out = Vec::new();
        codec.encode(Outbound::Pong, &mut out).unwrap();
        let v: ServerMessage = serde_json::from_slice(&out).unwrap();
        assert_eq!(v, ServerMessage::Pong);
    }
}
