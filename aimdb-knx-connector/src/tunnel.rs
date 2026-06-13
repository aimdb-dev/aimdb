//! Sans-io KNX/IP tunneling state machine (issue #135, design doc 034 §3.7)
//!
//! One implementation of the tunneling lifecycle — CONNECT_REQUEST/RESPONSE
//! handshake, TUNNELING_REQUEST/ACK sequence bookkeeping, keepalive
//! (CONNECTIONSTATE_REQUEST) scheduling, ACK-timeout sweeps, and
//! reconnect-with-backoff — shared by the tokio and Embassy transports.
//!
//! The engine owns no I/O and no clock. Transports feed it events
//! ([`TunnelEngine::handle_datagram`], [`TunnelEngine::handle_command`],
//! [`TunnelEngine::handle_socket_error`]), call [`TunnelEngine::poll`] with the
//! current monotonic time to fire elapsed deadlines, drain [`Action`]s via
//! [`TunnelEngine::next_action`], and arm their timer from
//! [`TunnelEngine::next_deadline`]. Time is `u64` milliseconds from an
//! arbitrary epoch chosen by the transport; only differences matter.
//!
//! Mirrors the `framing.rs` precedent in `aimdb-serial-connector`: pure
//! `no_std + alloc` protocol logic, runtime-specific socket glue around it.

use crate::GroupAddress;
use aimdb_core::transport::PublishError;
use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;
use knx_pico::protocol::{
    CEMIFrame, ConnectRequest, ConnectResponse, ConnectionHeader, ConnectionStateRequest,
    ConnectionStateResponse, Hpai, KnxnetIpFrame, ServiceType, TunnelingAck, TunnelingRequest,
};

/// Monotonic milliseconds from an arbitrary, transport-chosen epoch.
pub type Millis = u64;

/// Maximum KNX/IP APDU payload carried in one GroupValueWrite.
pub const MAX_APDU: usize = 254;

/// cEMI header (msg_code, add_info_len, ctrl1, ctrl2, src, dest, npdu_len,
/// tpci, apci) plus a full APDU.
const MAX_CEMI: usize = 12 + MAX_APDU;

/// KNX/IP frame header (6) + connection header (4) + a full cEMI frame,
/// rounded up. Every datagram the engine emits fits.
const MAX_FRAME: usize = 12 + MAX_CEMI;

/// A wire datagram to send to the gateway — stack-allocated.
pub type Frame = heapless::Vec<u8, MAX_FRAME>;

/// Outbound GroupValueWrite command, handed to the engine by the transport's
/// command channel (fed by the `Connector::publish` side).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupWrite {
    pub group_addr: GroupAddress,
    pub data: heapless::Vec<u8, MAX_APDU>,
}

impl GroupWrite {
    /// Validate one `Connector::publish` call into a tunnel command.
    ///
    /// Shared by both runtime shims so the checks (and their error
    /// precedence) cannot drift: destination first, then payload size.
    pub fn try_new(destination: &str, payload: &[u8]) -> Result<Self, PublishError> {
        let group_addr = destination
            .parse::<GroupAddress>()
            .map_err(|_| PublishError::InvalidDestination)?;
        let mut data = heapless::Vec::new();
        data.extend_from_slice(payload)
            .map_err(|_| PublishError::MessageTooLarge)?;
        Ok(Self { group_addr, data })
    }
}

/// What the transport must do next; drained via [`TunnelEngine::next_action`].
//
// `Send(Frame)` dominates the enum size by design: frames stay stack-allocated
// so the per-datagram path never touches the heap, and the action queue only
// ever holds a handful of entries.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Send this datagram to the gateway. `await_ack` carries the sequence
    /// number of a tracked TUNNELING_REQUEST so a failed send can stop its
    /// ACK tracking (see [`TunnelIo::send`]); `None` for everything else.
    Send { frame: Frame, await_ack: Option<u8> },
    /// Deliver a parsed inbound telegram toward `pump_source`
    /// (`try_send`, drop-on-full — never stall the protocol loop).
    Telegram {
        addr: GroupAddress,
        payload: Vec<u8>,
    },
    /// Tear down (and on the next connect attempt, re-create) the UDP socket.
    /// The engine has entered backoff and will emit the next CONNECT_REQUEST
    /// as a `Send` action once the backoff deadline passes.
    ResetSocket,
    /// An outbound telegram was never acknowledged: every send attempt
    /// (1 + [`TunnelConfig::ack_retransmits`]) expired unanswered. Log-only
    /// for the transport. With retransmits enabled the engine also tears the
    /// connection down afterwards (KNXnet/IP 3.8.4); with
    /// `ack_retransmits == 0` it keeps the connection, matching both
    /// pre-retransmit implementations.
    AckTimeout { seq: u8 },
}

/// Local HPAI advertised in CONNECT_REQUEST.
///
/// The previous tokio client sent its real bound address, the Embassy client
/// NAT mode — some gateways reject one or the other, so this stays a
/// per-transport choice instead of being unified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalEndpoint {
    /// `0.0.0.0:0` — the gateway replies to the datagram's source address.
    Nat,
    /// Real local address (required by some gateways).
    Explicit { ip: [u8; 4], port: u16 },
}

impl LocalEndpoint {
    fn hpai(&self) -> Hpai {
        match self {
            LocalEndpoint::Nat => Hpai::new([0, 0, 0, 0], 0),
            LocalEndpoint::Explicit { ip, port } => Hpai::new(*ip, *port),
        }
    }
}

/// Timing knobs and the local-endpoint policy. [`Default`] reproduces the
/// constants both previous implementations used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelConfig {
    pub local_endpoint: LocalEndpoint,
    /// CONNECT_RESPONSE wait before giving up and backing off.
    pub connect_timeout_ms: Millis,
    /// Pending ACK lifetime before a send attempt is considered unanswered.
    pub ack_timeout_ms: Millis,
    /// Cadence of the pending-ACK expiry sweep.
    pub ack_sweep_ms: Millis,
    /// TUNNELING_REQUEST retransmissions after an ACK timeout before the
    /// telegram is reported lost via [`Action::AckTimeout`].
    ///
    /// KNXnet/IP 3.8.4 repeats the identical frame once and tears the
    /// connection down when the repeat also goes unanswered — `1` (the
    /// default) is that behavior, with the retransmit delay being
    /// [`ack_timeout_ms`](Self::ack_timeout_ms) (set that to `1_000` for
    /// strict spec timing). `0` restores the pre-retransmit behavior: expire
    /// and warn only, no disconnect, and no frame bytes are buffered.
    pub ack_retransmits: u8,
    /// CONNECTIONSTATE_REQUEST keepalive cadence.
    pub heartbeat_ms: Millis,
    /// CONNECTIONSTATE_RESPONSE wait before the connection is considered
    /// dead (KNX spec CONNECTIONSTATE_REQUEST timeout: 10 s). This is the
    /// send-side liveness guard: a silently failing send path or a gateway
    /// that expired the tunnel never surfaces through the recv path of an
    /// unconnected UDP socket.
    pub heartbeat_response_timeout_ms: Millis,
    /// Wait between a connection failure and the next CONNECT_REQUEST.
    pub reconnect_backoff_ms: Millis,
}

impl Default for TunnelConfig {
    fn default() -> Self {
        Self {
            local_endpoint: LocalEndpoint::Nat,
            connect_timeout_ms: 5_000,
            ack_timeout_ms: 3_000,
            ack_sweep_ms: 500,
            ack_retransmits: 1,
            heartbeat_ms: 55_000,
            heartbeat_response_timeout_ms: 10_000,
            reconnect_backoff_ms: 5_000,
        }
    }
}

/// Pending outbound ACKs tracked per connection (seq → [`PendingAck`]).
const PENDING_ACK_CAPACITY: usize = 16;

/// A tracked outbound TUNNELING_REQUEST awaiting its TUNNELING_ACK.
#[derive(Debug)]
struct PendingAck {
    sent_at: Millis,
    /// Send attempts still allowed after the next expiry.
    retries_left: u8,
    /// The sent frame, buffered for byte-identical retransmission (KNXnet/IP
    /// 3.8.4 repeats the same frame, same sequence counter). Left empty when
    /// `ack_retransmits == 0`: the slot capacity is statically reserved
    /// either way (heapless), but no bytes are copied.
    frame: Frame,
}

/// Per-connection state; only exists while the handshake has succeeded, so
/// "connected" needs no separate flag and the keepalive cannot fire while
/// disconnected.
#[derive(Debug)]
struct ChannelState {
    channel_id: u8,
    outbound_seq: u8,
    pending_acks: heapless::FnvIndexMap<u8, PendingAck, PENDING_ACK_CAPACITY>,
    next_heartbeat: Millis,
    /// Set when a CONNECTIONSTATE_REQUEST goes out; cleared by the gateway's
    /// OK response. A pending entry older than
    /// `heartbeat_response_timeout_ms` drops the connection.
    heartbeat_pending_since: Option<Millis>,
    next_ack_sweep: Millis,
}

impl ChannelState {
    fn new(channel_id: u8, now: Millis, cfg: &TunnelConfig) -> Self {
        Self {
            channel_id,
            outbound_seq: 0,
            pending_acks: heapless::FnvIndexMap::new(),
            next_heartbeat: now + cfg.heartbeat_ms,
            heartbeat_pending_since: None,
            next_ack_sweep: now + cfg.ack_sweep_ms,
        }
    }

    fn next_outbound_seq(&mut self) -> u8 {
        let seq = self.outbound_seq;
        self.outbound_seq = self.outbound_seq.wrapping_add(1);
        seq
    }
}

// One `Phase` exists per engine, so the `Connected` payload size is irrelevant
// and boxing it would only add indirection.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
enum Phase {
    /// Waiting out the backoff before (re)connecting. The initial phase is
    /// `Backoff { until: now }`, so the first `poll` connects immediately.
    Backoff {
        until: Millis,
    },
    /// CONNECT_REQUEST sent, awaiting CONNECT_RESPONSE.
    Connecting {
        sent_at: Millis,
    },
    Connected(ChannelState),
}

/// The sans-io tunneling engine. See the module docs for the driving contract.
#[derive(Debug)]
pub struct TunnelEngine {
    cfg: TunnelConfig,
    phase: Phase,
    actions: VecDeque<Action>,
}

impl TunnelEngine {
    pub fn new(cfg: TunnelConfig, now: Millis) -> Self {
        Self {
            cfg,
            phase: Phase::Backoff { until: now },
            actions: VecDeque::new(),
        }
    }

    /// Update the HPAI used by future CONNECT_REQUESTs. Transports that
    /// advertise their real address call this after every (re)bind, before
    /// the backoff deadline fires.
    pub fn set_local_endpoint(&mut self, ep: LocalEndpoint) {
        self.cfg.local_endpoint = ep;
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.phase, Phase::Connected(_))
    }

    /// A datagram arrived from the gateway.
    pub fn handle_datagram(&mut self, data: &[u8], now: Millis) {
        // Set inside the `Connected` arm (where `self.phase` is borrowed) and
        // applied after the match.
        let mut drop_connection = false;
        match &mut self.phase {
            // Stale datagram from a previous connection — ignore.
            Phase::Backoff { .. } => {}
            Phase::Connecting { .. } => match parse_connect_response(data) {
                Some((channel_id, 0)) => {
                    self.phase = Phase::Connected(ChannelState::new(channel_id, now, &self.cfg));
                }
                Some((_, _status)) => {
                    // Gateway refused the connection — back off and retry.
                    self.actions.push_back(Action::ResetSocket);
                    self.phase = Phase::Backoff {
                        until: now + self.cfg.reconnect_backoff_ms,
                    };
                }
                // Not a CONNECT_RESPONSE (or unparseable): keep waiting;
                // the connect timeout covers a gateway that never answers.
                None => {}
            },
            Phase::Connected(state) => {
                let Ok(frame) = KnxnetIpFrame::parse(data) else {
                    return;
                };
                match frame.service_type() {
                    ServiceType::TunnellingAck => {
                        let body = frame.body();
                        let seq = if let Ok(ack) = TunnelingAck::parse(body) {
                            ack.connection_header.sequence_counter
                        } else if body.len() >= 4 {
                            // Some gateways omit the trailing status byte;
                            // the sequence sits in the connection header:
                            // [structure_len, channel_id, seq, flags].
                            body[2]
                        } else {
                            return;
                        };
                        // Unexpected (untracked) ACKs are ignored.
                        let _ = state.pending_acks.remove(&seq);
                    }
                    ServiceType::TunnellingRequest => {
                        // A parseable connection header is required before
                        // ACKing — truncated frames are dropped without an ACK
                        // (the gateway retransmits), never ACKed with a
                        // fabricated sequence number.
                        let Ok(request) = TunnelingRequest::parse(frame.body()) else {
                            return;
                        };
                        let seq = request.connection_header.sequence_counter;
                        let channel_id = state.channel_id;
                        self.actions.push_back(Action::Send {
                            frame: build_tunneling_ack(channel_id, seq),
                            await_ack: None,
                        });
                        if let Some((addr, payload)) = parse_telegram(request.cemi_data) {
                            self.actions.push_back(Action::Telegram { addr, payload });
                        }
                    }
                    ServiceType::ConnectionstateResponse => {
                        let Ok(response) = ConnectionStateResponse::parse(frame.body()) else {
                            return;
                        };
                        if response.is_ok() {
                            state.heartbeat_pending_since = None;
                        } else {
                            // The gateway no longer recognizes the channel
                            // (e.g. it expired during an outage) — reconnect
                            // with a fresh one.
                            drop_connection = true;
                        }
                    }
                    // DISCONNECT_RESPONSE, …
                    _ => {}
                }
            }
        }
        if drop_connection {
            self.handle_socket_error(now);
        }
    }

    /// An outbound GroupValueWrite arrived from the command channel.
    ///
    /// Returns `false` when the command was dropped because no connection is
    /// established. This is a defensive contract: both shims gate command
    /// intake on `is_connected()` (commands queue in their channel while
    /// disconnected), so in production this path is unreachable — it is
    /// exercised by the engine unit tests only.
    pub fn handle_command(&mut self, cmd: GroupWrite, now: Millis) -> bool {
        let Phase::Connected(state) = &mut self.phase else {
            return false;
        };
        let seq = state.next_outbound_seq();
        let cemi = build_group_write_cemi(cmd.group_addr, &cmd.data);
        let frame = build_tunneling_request(state.channel_id, seq, &cemi);
        let pending = PendingAck {
            sent_at: now,
            retries_left: self.cfg.ack_retransmits,
            frame: if self.cfg.ack_retransmits > 0 {
                frame.clone()
            } else {
                Frame::new()
            },
        };
        if let Err((_, pending)) = state.pending_acks.insert(seq, pending) {
            // Map full (burst deeper than PENDING_ACK_CAPACITY): evict the
            // oldest entry and report it now, so no unacknowledged telegram
            // is ever silently untracked. Eviction is overflow, not confirmed
            // loss, so it never tears the connection down.
            if let Some((&oldest, _)) = state.pending_acks.iter().next() {
                state.pending_acks.remove(&oldest);
                self.actions.push_back(Action::AckTimeout { seq: oldest });
            }
            let _ = state.pending_acks.insert(seq, pending);
        }
        self.actions.push_back(Action::Send {
            frame,
            await_ack: Some(seq),
        });
        true
    }

    /// The transport failed to hand a tracked TUNNELING_REQUEST to the socket
    /// (see [`TunnelIo::send`]): stop awaiting its ACK so the sweep doesn't
    /// misreport the local send failure as a gateway ACK loss.
    fn send_failed(&mut self, seq: u8) {
        if let Phase::Connected(state) = &mut self.phase {
            let _ = state.pending_acks.remove(&seq);
        }
    }

    /// The transport's UDP send/recv failed fatally — drop the connection and
    /// start a reconnect cycle.
    pub fn handle_socket_error(&mut self, now: Millis) {
        // Always tear the erroring socket down (the previous implementations
        // recreated it on every recv error); an already-running backoff keeps
        // its deadline so persistent errors cannot postpone the reconnect.
        self.actions.push_back(Action::ResetSocket);
        if !matches!(self.phase, Phase::Backoff { .. }) {
            self.phase = Phase::Backoff {
                until: now + self.cfg.reconnect_backoff_ms,
            };
        }
    }

    /// Fire any deadlines that have passed. Call at the top of every loop
    /// iteration, before draining actions.
    pub fn poll(&mut self, now: Millis) {
        // Set inside the `Connected` arm (where `self.phase` is borrowed) and
        // applied after the match — same pattern as `handle_datagram`.
        let mut drop_connection = false;
        match &mut self.phase {
            Phase::Backoff { until } => {
                if now >= *until {
                    let frame = build_connect_request(&self.cfg.local_endpoint);
                    self.actions.push_back(Action::Send {
                        frame,
                        await_ack: None,
                    });
                    self.phase = Phase::Connecting { sent_at: now };
                }
            }
            Phase::Connecting { sent_at } => {
                if now >= sent_at.saturating_add(self.cfg.connect_timeout_ms) {
                    self.actions.push_back(Action::ResetSocket);
                    self.phase = Phase::Backoff {
                        until: now + self.cfg.reconnect_backoff_ms,
                    };
                }
            }
            Phase::Connected(state) => {
                // Unanswered keepalive: the gateway (or the send path) is
                // gone — drop the connection and re-handshake.
                if let Some(sent_at) = state.heartbeat_pending_since {
                    if now >= sent_at.saturating_add(self.cfg.heartbeat_response_timeout_ms) {
                        self.handle_socket_error(now);
                        return;
                    }
                }
                if now >= state.next_heartbeat {
                    let frame = build_connectionstate_request(state.channel_id);
                    self.actions.push_back(Action::Send {
                        frame,
                        await_ack: None,
                    });
                    if state.heartbeat_pending_since.is_none() {
                        state.heartbeat_pending_since = Some(now);
                    }
                    state.next_heartbeat = now + self.cfg.heartbeat_ms;
                }
                if now >= state.next_ack_sweep {
                    let mut expired: heapless::Vec<u8, PENDING_ACK_CAPACITY> = heapless::Vec::new();
                    for (&seq, pending) in state.pending_acks.iter() {
                        if now.saturating_sub(pending.sent_at) > self.cfg.ack_timeout_ms {
                            let _ = expired.push(seq);
                        }
                    }
                    for seq in &expired {
                        let Some(pending) = state.pending_acks.get_mut(seq) else {
                            continue;
                        };
                        if pending.retries_left > 0 {
                            // KNXnet/IP 3.8.4: repeat the identical frame
                            // (same sequence counter) and re-arm the timeout.
                            pending.retries_left -= 1;
                            pending.sent_at = now;
                            self.actions.push_back(Action::Send {
                                frame: pending.frame.clone(),
                                await_ack: Some(*seq),
                            });
                        } else {
                            state.pending_acks.remove(seq);
                            self.actions.push_back(Action::AckTimeout { seq: *seq });
                            // Spec: tear the connection down once the repeat
                            // also went unanswered, so queued commands stop
                            // being sent into a dead tunnel.
                            // `ack_retransmits == 0` keeps the legacy
                            // warn-and-continue behavior.
                            if self.cfg.ack_retransmits > 0 {
                                drop_connection = true;
                            }
                        }
                    }
                    state.next_ack_sweep = now + self.cfg.ack_sweep_ms;
                }
            }
        }
        if drop_connection {
            self.handle_socket_error(now);
        }
    }

    /// Earliest deadline the transport must wake the engine for.
    pub fn next_deadline(&self) -> Millis {
        match &self.phase {
            Phase::Backoff { until } => *until,
            Phase::Connecting { sent_at } => sent_at.saturating_add(self.cfg.connect_timeout_ms),
            Phase::Connected(state) => {
                let mut next = state.next_heartbeat.min(state.next_ack_sweep);
                if let Some(sent_at) = state.heartbeat_pending_since {
                    next = next.min(sent_at.saturating_add(self.cfg.heartbeat_response_timeout_ms));
                }
                next
            }
        }
    }

    /// Drain pending actions (typically 0–2 per event).
    pub fn next_action(&mut self) -> Option<Action> {
        self.actions.pop_front()
    }
}

// ---------------------------------------------------------------------------
// Action drain — the runtime-neutral half of every transport loop.
// ---------------------------------------------------------------------------

/// Transport glue [`drain_actions`] drives: an async datagram send plus the
/// non-blocking inbound forward. Implemented once per runtime shim; the policy
/// for applying [`Action`]s lives in [`drain_actions`] so it cannot drift
/// between the tokio and Embassy loops.
// Unused only when the crate is built without either runtime shim.
#[cfg_attr(
    not(any(feature = "tokio-runtime", feature = "embassy-runtime")),
    allow(dead_code)
)]
pub(crate) trait TunnelIo {
    /// Send one datagram to the gateway. Returns `false` when the datagram
    /// could not be handed to the socket; [`drain_actions`] then stops the
    /// ACK tracking of a tracked TUNNELING_REQUEST. Transient errors are
    /// logged by the impl and otherwise non-fatal — an established tunnel
    /// must survive e.g. ENOBUFS or a route flap. A persistently dead send
    /// path surfaces through the heartbeat-response timeout
    /// ([`TunnelConfig::heartbeat_response_timeout_ms`]), which drops the
    /// connection even when the recv path never errors.
    async fn send(&mut self, frame: &[u8]) -> bool;
    /// Forward a parsed telegram toward `pump_source`. Non-blocking:
    /// drop + log on a full channel rather than stalling the protocol loop.
    fn forward(&mut self, addr: GroupAddress, payload: Vec<u8>);
    /// An outbound telegram's ACK never arrived (log-only, see
    /// [`Action::AckTimeout`]).
    fn warn_ack_timeout(&mut self, seq: u8);
}

/// Apply every pending engine action through `io`. Returns `true` when the
/// engine asked for the socket to be torn down ([`Action::ResetSocket`]); the
/// transport reacts once the drain completes.
#[cfg_attr(
    not(any(feature = "tokio-runtime", feature = "embassy-runtime")),
    allow(dead_code)
)]
pub(crate) async fn drain_actions(engine: &mut TunnelEngine, io: &mut impl TunnelIo) -> bool {
    let mut reset = false;
    while let Some(action) = engine.next_action() {
        match action {
            Action::Send { frame, await_ack } => {
                if !io.send(&frame).await {
                    if let Some(seq) = await_ack {
                        engine.send_failed(seq);
                    }
                }
            }
            Action::Telegram { addr, payload } => io.forward(addr, payload),
            Action::ResetSocket => reset = true,
            Action::AckTimeout { seq } => io.warn_ack_timeout(seq),
        }
    }
    reset
}

// ---------------------------------------------------------------------------
// Frame building / parsing — moved verbatim from the former tokio/embassy
// clients (logging stripped; byte layouts unchanged).
// ---------------------------------------------------------------------------

fn frame_from(buf: &[u8]) -> Frame {
    let mut frame = Frame::new();
    frame
        .extend_from_slice(buf)
        .expect("frame buffer sized for max KNX/IP datagram");
    frame
}

/// Build CONNECT_REQUEST advertising the configured local HPAI.
fn build_connect_request(ep: &LocalEndpoint) -> Frame {
    let hpai = ep.hpai();
    let request = ConnectRequest::new(hpai, hpai);
    let mut buffer = [0u8; 32];
    let len = request
        .build(&mut buffer)
        .expect("buffer sized for CONNECT_REQUEST");
    frame_from(&buffer[..len])
}

/// Extract `(channel_id, status)` if `data` is a CONNECT_RESPONSE.
fn parse_connect_response(data: &[u8]) -> Option<(u8, u8)> {
    let frame = KnxnetIpFrame::parse(data).ok()?;
    if frame.service_type() != ServiceType::ConnectResponse {
        return None;
    }
    let response = ConnectResponse::parse(frame.body()).ok()?;
    Some((response.channel_id, response.status))
}

fn build_tunneling_ack(channel_id: u8, seq: u8) -> Frame {
    let conn_header = ConnectionHeader::new(channel_id, seq);
    let ack = TunnelingAck::new(conn_header, 0); // status = 0 (OK)
    let mut buffer = [0u8; 16];
    let len = ack
        .build(&mut buffer)
        .expect("buffer sized for TUNNELING_ACK");
    frame_from(&buffer[..len])
}

fn build_tunneling_request(channel_id: u8, seq: u8, cemi: &[u8]) -> Frame {
    let conn_header = ConnectionHeader::new(channel_id, seq);
    let request = TunnelingRequest::new(conn_header, cemi);
    let mut buffer = [0u8; MAX_FRAME];
    let len = request
        .build(&mut buffer)
        .expect("buffer sized for max TUNNELING_REQUEST");
    frame_from(&buffer[..len])
}

fn build_connectionstate_request(channel_id: u8) -> Frame {
    // 0.0.0.0:0 ("any") — matches both previous implementations.
    let hpai = Hpai::new([0, 0, 0, 0], 0);
    let request = ConnectionStateRequest::new(channel_id, hpai);
    let mut buffer = [0u8; 32];
    let len = request
        .build(&mut buffer)
        .expect("buffer sized for CONNECTIONSTATE_REQUEST");
    frame_from(&buffer[..len])
}

/// Build GroupValueWrite cEMI frame (L_Data.req)
///
/// Must match knx-pico's exact cEMI structure for proper parsing.
/// Structure: [msg_code, add_info_len, ctrl1, ctrl2, src(2), dest(2), npdu_len, tpci, apci, data...]
fn build_group_write_cemi(group_addr: GroupAddress, data: &[u8]) -> heapless::Vec<u8, MAX_CEMI> {
    let mut frame = heapless::Vec::new();

    // Message code: L_Data.req (0x11)
    let _ = frame.push(0x11);

    // Additional info length: 0
    let _ = frame.push(0x00);

    // Control field 1: 0xBC (Standard frame, no repeat, broadcast, priority low)
    // Use 0xBC instead of 0x94 - this is critical for gateway compatibility
    let _ = frame.push(0xBC);

    // Control field 2: 0xE0 (Group address, hop count 6)
    let _ = frame.push(0xE0);

    // Source address: 0.0.0 (2 bytes, big-endian)
    let _ = frame.extend_from_slice(&[0x00, 0x00]);

    // Destination address (group address) - convert to u16 big-endian
    let dest_raw: u16 = group_addr.into();
    let dest_bytes = dest_raw.to_be_bytes();
    let _ = frame.extend_from_slice(&dest_bytes);

    // Build NPDU: NPDU_length field + TPCI + APCI + data
    // CRITICAL: NPDU length encoding per KNX spec:
    // - For short telegram: field = 0x01 (special flag)
    // - For long telegram: field = actual_length - 1 (encoded as length-1)
    if data.len() == 1 && data[0] < 64 {
        // 6-bit encoding: value embedded in APCI byte
        // NPDU length = 0x01 (short telegram flag, NOT byte count)
        let _ = frame.push(0x01);

        // TPCI (UnnumberedData)
        let _ = frame.push(0x00);

        // APCI low byte: GroupValueWrite (0x80) + 6-bit value
        let _ = frame.push(0x80 | (data[0] & 0x3F));
    } else {
        // Long telegram: APCI + separate data bytes
        // NPDU length encoding: field = actual_length - 1
        let npdu_actual = 2 + data.len(); // TPCI + APCI + data
        let npdu_len_field = npdu_actual - 1; // Encode as length - 1
        let _ = frame.push(npdu_len_field as u8);

        // TPCI (UnnumberedData)
        let _ = frame.push(0x00);

        // APCI: GroupValueWrite
        let _ = frame.push(0x80);

        // Data bytes
        let _ = frame.extend_from_slice(data);
    }

    frame
}

/// Parse a cEMI frame — the body of an already-validated TUNNELING_REQUEST,
/// handed over by `handle_datagram` so the datagram is parsed exactly once.
///
/// Returns (group_address, payload) if this is a valid L_Data telegram
/// carrying a GroupValueWrite to a group address.
fn parse_telegram(cemi_data: &[u8]) -> Option<(GroupAddress, Vec<u8>)> {
    // Parse cEMI frame
    let cemi = CEMIFrame::parse(cemi_data).ok()?;

    // Only process L_Data frames
    if !cemi.is_ldata() {
        return None;
    }

    // Parse LData frame using knx-pico (handles all encoding variants including 6-bit values)
    let ldata = cemi.as_ldata().ok()?;

    // Only process group write commands
    if !ldata.is_group_write() {
        return None;
    }

    // Only process group addresses (not individual addresses)
    let dest = ldata.destination_group()?;

    // Extract payload (application data)
    // For 6-bit encoded values (DPT1 boolean), ldata.data is empty
    // and the value is encoded in the APCI byte. We need to extract it manually.
    // Note: npdu_length can be 1 (combined TPCI+APCI) or 2 (separate TPCI and APCI)
    let payload = if ldata.data.is_empty() {
        // 6-bit encoding: extract value from APCI byte in raw cEMI data
        // cEMI structure: [msg_code, add_info_len, <add_info>, ctrl1, ctrl2, src(2), dest(2), npdu_len, tpci, apci, ...]
        // APCI byte position = 2 + add_info_len + 6 (ctrl1, ctrl2, src(2), dest(2), npdu_len) + 1 (tpci) = 2 + add_info_len + 7 + 1
        let add_info_len = if cemi_data.len() > 1 { cemi_data[1] } else { 0 } as usize;
        let apci_pos = 2 + add_info_len + 8; // TPCI is at +7, APCI is at +8

        if cemi_data.len() > apci_pos {
            let apci_byte = cemi_data[apci_pos];
            let value = apci_byte & 0x3F; // Extract 6-bit value
            vec![value]
        } else {
            vec![]
        }
    } else {
        // Standard encoding: multi-byte data (DPT5, DPT7, DPT9, etc.)
        //
        // cEMI L_Data structure (after msg_code and add_info):
        // [0] ctrl1, [1] ctrl2, [2-3] src, [4-5] dest, [6] npdu_len, [7] TPCI, [8] APCI_low, [9+] data
        //
        // According to knx-pico parser: data starts at position 9 in L_Data
        // In full cEMI frame: position = 2 + add_info_len + 9 = 11 (when add_info_len=0)
        let add_info_len = if cemi_data.len() > 1 { cemi_data[1] } else { 0 } as usize;

        // Data starts at: msg_code(0) + add_info_len_field(1) + add_info(variable) + L_Data_header(9)
        let ldata_offset = 2 + add_info_len;
        let data_start = ldata_offset + 9; // Position 11 when add_info_len=0

        if cemi_data.len() > data_start {
            cemi_data[data_start..].to_vec()
        } else {
            // Fallback to knx-pico's parsed data if extraction fails
            ldata.data.to_vec()
        }
    };

    Some((dest, payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Legacy-mode config: no retransmits, expire-and-warn only. Most tests
    /// pin this pre-retransmit contract; the retransmit tests use
    /// [`RETRANSMIT_CFG`].
    const CFG: TunnelConfig = TunnelConfig {
        local_endpoint: LocalEndpoint::Nat,
        connect_timeout_ms: 5_000,
        ack_timeout_ms: 3_000,
        ack_sweep_ms: 500,
        ack_retransmits: 0,
        heartbeat_ms: 55_000,
        heartbeat_response_timeout_ms: 10_000,
        reconnect_backoff_ms: 5_000,
    };

    /// [`CFG`] with the spec-conformant retransmit knob (the shipping
    /// default) enabled.
    const RETRANSMIT_CFG: TunnelConfig = TunnelConfig {
        ack_retransmits: 1,
        ..CFG
    };

    fn drain(engine: &mut TunnelEngine) -> Vec<Action> {
        let mut actions = Vec::new();
        while let Some(a) = engine.next_action() {
            actions.push(a);
        }
        actions
    }

    fn service_type_of(frame: &[u8]) -> u16 {
        u16::from_be_bytes([frame[2], frame[3]])
    }

    /// Hand-built CONNECT_RESPONSE: header + [channel_id, status, HPAI(8), CRD(4)].
    fn connect_response(channel_id: u8, status: u8) -> Vec<u8> {
        let mut body = vec![channel_id, status];
        body.extend_from_slice(&[0x08, 0x01, 0, 0, 0, 0, 0, 0]); // HPAI 0.0.0.0:0
        body.extend_from_slice(&[0x04, 0x04, 0x02, 0x00]); // CRD: tunnel, addr 0.0.0
        let mut frame = vec![0x06, 0x10, 0x02, 0x06];
        frame.extend_from_slice(&(6 + body.len() as u16).to_be_bytes());
        frame.extend_from_slice(&body);
        frame
    }

    /// A gateway-side TUNNELING_REQUEST carrying a GroupValueWrite.
    fn inbound_group_write(channel_id: u8, seq: u8, addr: GroupAddress, data: &[u8]) -> Vec<u8> {
        let cemi = build_group_write_cemi(addr, data);
        build_tunneling_request(channel_id, seq, &cemi).to_vec()
    }

    /// Standard 5-byte-body TUNNELING_ACK from the gateway.
    fn gateway_ack(channel_id: u8, seq: u8) -> Vec<u8> {
        build_tunneling_ack(channel_id, seq).to_vec()
    }

    /// Drive a fresh engine to Connected; returns it with actions drained.
    fn connected_engine(now: Millis) -> TunnelEngine {
        connected_engine_with(CFG.clone(), now)
    }

    /// [`connected_engine`] with an explicit config.
    fn connected_engine_with(cfg: TunnelConfig, now: Millis) -> TunnelEngine {
        let mut engine = TunnelEngine::new(cfg, now);
        engine.poll(now);
        let actions = drain(&mut engine);
        assert_eq!(actions.len(), 1);
        let Action::Send { frame: req, .. } = &actions[0] else {
            panic!("expected CONNECT_REQUEST send, got {actions:?}");
        };
        assert_eq!(service_type_of(req), 0x0205); // CONNECT_REQUEST
        engine.handle_datagram(&connect_response(7, 0), now);
        assert!(engine.is_connected());
        engine
    }

    #[test]
    fn handshake_connects_on_status_ok() {
        let engine = connected_engine(1_000);
        assert_eq!(engine.next_deadline(), 1_000 + CFG.ack_sweep_ms);
    }

    #[test]
    fn handshake_refused_backs_off_and_retries() {
        let mut engine = TunnelEngine::new(CFG.clone(), 0);
        engine.poll(0);
        drain(&mut engine);
        engine.handle_datagram(&connect_response(7, 0x24), 10); // E_NO_MORE_CONNECTIONS
        assert!(!engine.is_connected());
        assert_eq!(drain(&mut engine), vec![Action::ResetSocket]);
        assert_eq!(engine.next_deadline(), 10 + CFG.reconnect_backoff_ms);

        // Backoff expiry emits a fresh CONNECT_REQUEST.
        engine.poll(10 + CFG.reconnect_backoff_ms);
        let actions = drain(&mut engine);
        assert!(
            matches!(&actions[..], [Action::Send { frame: f, .. }] if service_type_of(f) == 0x0205)
        );
    }

    #[test]
    fn connect_timeout_resets_and_backs_off() {
        let mut engine = TunnelEngine::new(CFG.clone(), 0);
        engine.poll(0);
        drain(&mut engine);
        assert_eq!(engine.next_deadline(), CFG.connect_timeout_ms);

        // Nothing happens before the deadline…
        engine.poll(CFG.connect_timeout_ms - 1);
        assert!(drain(&mut engine).is_empty());

        // …and the timeout tears down the socket and backs off.
        engine.poll(CFG.connect_timeout_ms);
        assert_eq!(drain(&mut engine), vec![Action::ResetSocket]);
        engine.poll(CFG.connect_timeout_ms + CFG.reconnect_backoff_ms);
        let actions = drain(&mut engine);
        assert!(
            matches!(&actions[..], [Action::Send { frame: f, .. }] if service_type_of(f) == 0x0205)
        );
    }

    #[test]
    fn truncated_tunneling_request_is_not_acked() {
        let mut engine = connected_engine(0);

        // Header-only / truncated-connection-header TUNNELING_REQUESTs: no
        // sequence number to echo, so no ACK may be fabricated (seq 0 would
        // desynchronize the gateway's tunnel sequence tracking).
        for frame in [
            vec![0x06, 0x10, 0x04, 0x20, 0x00, 0x06], // header only
            vec![0x06, 0x10, 0x04, 0x20, 0x00, 0x08, 0x04, 0x07], // 2-byte body
        ] {
            engine.handle_datagram(&frame, 100);
        }

        assert!(drain(&mut engine).is_empty());
        assert!(engine.is_connected());
    }

    #[test]
    fn unparseable_datagram_while_connecting_is_ignored() {
        let mut engine = TunnelEngine::new(CFG.clone(), 0);
        engine.poll(0);
        drain(&mut engine);
        engine.handle_datagram(&[0xde, 0xad], 1);
        engine.handle_datagram(&gateway_ack(7, 0), 2); // wrong service type
        assert!(!engine.is_connected());
        assert!(drain(&mut engine).is_empty());
        // Still waiting — and still connectable.
        engine.handle_datagram(&connect_response(3, 0), 3);
        assert!(engine.is_connected());
    }

    #[test]
    fn inbound_telegram_is_acked_and_routed() {
        let addr: GroupAddress = "1/0/7".parse().unwrap();
        let mut engine = connected_engine(0);

        // Multi-byte payload (e.g. DPT9 temperature).
        let datagram = inbound_group_write(7, 42, addr, &[0x0C, 0x1A]);
        engine.handle_datagram(&datagram, 100);
        let actions = drain(&mut engine);
        assert_eq!(actions.len(), 2);
        let Action::Send { frame: ack, .. } = &actions[0] else {
            panic!("expected ACK first, got {actions:?}");
        };
        assert_eq!(service_type_of(ack), 0x0421); // TUNNELING_ACK
        assert_eq!(ack[7], 7); // channel_id echoed
        assert_eq!(ack[8], 42); // sequence echoed
        assert_eq!(
            actions[1],
            Action::Telegram {
                addr,
                payload: vec![0x0C, 0x1A]
            }
        );
    }

    #[test]
    fn inbound_6bit_telegram_extracts_apci_value() {
        let addr: GroupAddress = "2/1/3".parse().unwrap();
        let mut engine = connected_engine(0);

        // Single-byte value < 64 is encoded in the APCI byte (DPT1 boolean).
        let datagram = inbound_group_write(7, 1, addr, &[0x01]);
        engine.handle_datagram(&datagram, 100);
        let actions = drain(&mut engine);
        assert_eq!(
            actions[1],
            Action::Telegram {
                addr,
                payload: vec![0x01]
            }
        );
    }

    #[test]
    fn outbound_commands_use_wrapping_sequence_numbers() {
        let addr: GroupAddress = "1/0/8".parse().unwrap();
        let mut engine = connected_engine(0);

        for expected_seq in [0u8, 1] {
            let mut data = heapless::Vec::new();
            data.push(0x01).unwrap();
            assert!(engine.handle_command(
                GroupWrite {
                    group_addr: addr,
                    data
                },
                10
            ));
            let actions = drain(&mut engine);
            let Action::Send { frame, .. } = &actions[0] else {
                panic!("expected TUNNELING_REQUEST send");
            };
            assert_eq!(service_type_of(frame), 0x0420);
            assert_eq!(frame[8], expected_seq);
        }
    }

    #[test]
    fn outbound_seq_wraps_at_255() {
        let addr: GroupAddress = "1/0/8".parse().unwrap();
        let mut engine = connected_engine(0);
        // Pre-wind the counter via the internal state (exercising 256 sends
        // would also work but is slow to read).
        let Phase::Connected(state) = &mut engine.phase else {
            unreachable!()
        };
        state.outbound_seq = 255;
        let mut data = heapless::Vec::new();
        data.push(0x01).unwrap();
        engine.handle_command(
            GroupWrite {
                group_addr: addr,
                data: data.clone(),
            },
            10,
        );
        engine.handle_datagram(&gateway_ack(7, 255), 11);
        engine.handle_command(
            GroupWrite {
                group_addr: addr,
                data,
            },
            12,
        );
        let actions = drain(&mut engine);
        let seqs: Vec<u8> = actions
            .iter()
            .filter_map(|a| match a {
                Action::Send { frame: f, .. } if service_type_of(f) == 0x0420 => Some(f[8]),
                _ => None,
            })
            .collect();
        assert_eq!(seqs, vec![255, 0]);
    }

    #[test]
    fn command_while_disconnected_is_dropped() {
        let mut engine = TunnelEngine::new(CFG.clone(), 0);
        let mut data = heapless::Vec::new();
        data.push(0x01).unwrap();
        assert!(!engine.handle_command(
            GroupWrite {
                group_addr: "1/0/8".parse().unwrap(),
                data
            },
            0
        ));
        assert!(drain(&mut engine).is_empty());
    }

    #[test]
    fn standard_and_fallback_ack_bodies_complete_pending() {
        let addr: GroupAddress = "1/0/8".parse().unwrap();
        let mut engine = connected_engine(0);
        let mut data = heapless::Vec::new();
        data.push(0x01).unwrap();
        engine.handle_command(
            GroupWrite {
                group_addr: addr,
                data: data.clone(),
            },
            10,
        );
        engine.handle_command(
            GroupWrite {
                group_addr: addr,
                data,
            },
            10,
        );
        drain(&mut engine);

        // Standard 5-byte body for seq 0.
        engine.handle_datagram(&gateway_ack(7, 0), 20);
        // Non-standard 4-byte body (status byte missing) for seq 1.
        let mut short_ack = gateway_ack(7, 1);
        short_ack.truncate(short_ack.len() - 1);
        short_ack[4..6].copy_from_slice(&10u16.to_be_bytes()); // fix total length
        engine.handle_datagram(&short_ack, 21);

        // Both completed: no AckTimeout at the sweep after the ACK window.
        engine.poll(10 + CFG.ack_timeout_ms + CFG.ack_sweep_ms + 1);
        assert!(drain(&mut engine)
            .iter()
            .all(|a| !matches!(a, Action::AckTimeout { .. })));
    }

    #[test]
    fn unacked_telegram_expires_at_sweep() {
        let addr: GroupAddress = "1/0/8".parse().unwrap();
        let mut engine = connected_engine(0);
        let mut data = heapless::Vec::new();
        data.push(0x01).unwrap();
        engine.handle_command(
            GroupWrite {
                group_addr: addr,
                data,
            },
            10,
        );
        drain(&mut engine);

        // Sweeps inside the ACK window keep the entry pending.
        engine.poll(500);
        engine.poll(1_000);
        assert!(drain(&mut engine).is_empty());

        // First sweep past sent_at + ack_timeout expires it. Legacy mode
        // (ack_retransmits == 0): warn only, the connection is kept.
        engine.poll(10 + CFG.ack_timeout_ms + CFG.ack_sweep_ms);
        let actions = drain(&mut engine);
        assert_eq!(actions, vec![Action::AckTimeout { seq: 0 }]);
        assert!(engine.is_connected());
    }

    #[test]
    fn ack_timeout_retransmits_identical_frame_then_disconnects() {
        let addr: GroupAddress = "1/0/8".parse().unwrap();
        let mut engine = connected_engine_with(RETRANSMIT_CFG.clone(), 0);
        let mut data = heapless::Vec::new();
        data.push(0x01).unwrap();
        engine.handle_command(
            GroupWrite {
                group_addr: addr,
                data,
            },
            10,
        );
        let actions = drain(&mut engine);
        let [Action::Send {
            frame: original,
            await_ack: Some(0),
        }] = &actions[..]
        else {
            panic!("expected tracked TUNNELING_REQUEST send, got {actions:?}");
        };
        let original = original.clone();

        // First expiry: the identical frame goes out again (same sequence
        // counter, KNXnet/IP 3.8.4), no AckTimeout, connection kept.
        engine.poll(10 + CFG.ack_timeout_ms + CFG.ack_sweep_ms);
        let actions = drain(&mut engine);
        assert_eq!(
            actions,
            vec![Action::Send {
                frame: original,
                await_ack: Some(0),
            }]
        );
        assert!(engine.is_connected());

        // Second expiry: reported lost, and the connection is torn down so
        // subsequent commands queue instead of vanishing into a dead tunnel.
        let t = 10 + 2 * (CFG.ack_timeout_ms + CFG.ack_sweep_ms);
        engine.poll(t);
        let actions = drain(&mut engine);
        assert_eq!(
            actions,
            vec![Action::AckTimeout { seq: 0 }, Action::ResetSocket]
        );
        assert!(!engine.is_connected());
        assert_eq!(engine.next_deadline(), t + CFG.reconnect_backoff_ms);
    }

    #[test]
    fn retransmitted_request_acked_keeps_connection() {
        let addr: GroupAddress = "1/0/8".parse().unwrap();
        let mut engine = connected_engine_with(RETRANSMIT_CFG.clone(), 0);
        let mut data = heapless::Vec::new();
        data.push(0x01).unwrap();
        engine.handle_command(
            GroupWrite {
                group_addr: addr,
                data,
            },
            10,
        );
        drain(&mut engine);

        // Let the first send expire and the retransmit go out…
        engine.poll(10 + CFG.ack_timeout_ms + CFG.ack_sweep_ms);
        assert_eq!(drain(&mut engine).len(), 1);

        // …then the (late) ACK lands: pending cleared, no AckTimeout at any
        // later sweep, connection kept.
        engine.handle_datagram(&gateway_ack(7, 0), 4_000);
        engine.poll(20_000);
        assert!(drain(&mut engine).is_empty());
        assert!(engine.is_connected());
    }

    #[test]
    fn heartbeat_fires_on_cadence_only_while_connected() {
        let mut engine = connected_engine(0);

        engine.poll(CFG.heartbeat_ms - 1);
        assert!(drain(&mut engine)
            .iter()
            .all(|a| !matches!(a, Action::Send { frame: f, .. } if service_type_of(f) == 0x0207)));

        engine.poll(CFG.heartbeat_ms);
        let actions = drain(&mut engine);
        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::Send { frame: f, .. } if service_type_of(f) == 0x0207)));

        // After a socket error, no heartbeat fires during backoff.
        engine.handle_socket_error(CFG.heartbeat_ms + 10);
        drain(&mut engine);
        engine.poll(CFG.heartbeat_ms * 2);
        assert!(drain(&mut engine)
            .iter()
            .all(|a| !matches!(a, Action::Send { frame: f, .. } if service_type_of(f) == 0x0207)));
    }

    #[test]
    fn socket_error_triggers_reset_and_reconnect() {
        let mut engine = connected_engine(0);
        engine.handle_socket_error(100);
        assert!(!engine.is_connected());
        assert_eq!(drain(&mut engine), vec![Action::ResetSocket]);

        // A second error during backoff still tears the erroring socket down,
        // but the original backoff deadline stands (no postponement).
        engine.handle_socket_error(200);
        assert_eq!(drain(&mut engine), vec![Action::ResetSocket]);

        engine.poll(100 + CFG.reconnect_backoff_ms);
        let actions = drain(&mut engine);
        assert!(
            matches!(&actions[..], [Action::Send { frame: f, .. }] if service_type_of(f) == 0x0205)
        );
        engine.handle_datagram(&connect_response(9, 0), 100 + CFG.reconnect_backoff_ms + 5);
        assert!(engine.is_connected());
    }

    /// Hand-built CONNECTIONSTATE_RESPONSE (0x0208) frame.
    fn connectionstate_response(channel_id: u8, status: u8) -> Vec<u8> {
        let mut frame = vec![0x06, 0x10, 0x02, 0x08];
        frame.extend_from_slice(&8u16.to_be_bytes());
        frame.extend_from_slice(&[channel_id, status]);
        frame
    }

    #[test]
    fn ok_connectionstate_and_disconnect_responses_keep_connection() {
        let mut engine = connected_engine(0);
        engine.handle_datagram(&connectionstate_response(7, 0), 50);
        // DISCONNECT_RESPONSE (0x020A) is ignored.
        let mut frame = vec![0x06, 0x10, 0x02, 0x0A];
        frame.extend_from_slice(&8u16.to_be_bytes());
        frame.extend_from_slice(&[7, 0]);
        engine.handle_datagram(&frame, 50);
        assert!(engine.is_connected());
        assert!(drain(&mut engine).is_empty());
    }

    #[test]
    fn answered_heartbeat_keeps_connection_past_response_timeout() {
        let mut engine = connected_engine(0);
        engine.poll(CFG.heartbeat_ms);
        drain(&mut engine);
        engine.handle_datagram(&connectionstate_response(7, 0), CFG.heartbeat_ms + 100);
        engine.poll(CFG.heartbeat_ms + CFG.heartbeat_response_timeout_ms + 1);
        assert!(engine.is_connected());
        assert!(drain(&mut engine).is_empty());
    }

    #[test]
    fn unanswered_heartbeat_drops_connection_after_response_timeout() {
        let mut engine = connected_engine(0);
        engine.poll(CFG.heartbeat_ms);
        let actions = drain(&mut engine);
        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::Send { frame: f, .. } if service_type_of(f) == 0x0207)));

        // Inside the response window nothing happens…
        engine.poll(CFG.heartbeat_ms + CFG.heartbeat_response_timeout_ms - 1);
        assert!(engine.is_connected());
        assert!(drain(&mut engine).is_empty());

        // …then the silent gateway (or dead send path) drops the connection.
        engine.poll(CFG.heartbeat_ms + CFG.heartbeat_response_timeout_ms);
        assert!(!engine.is_connected());
        assert_eq!(drain(&mut engine), vec![Action::ResetSocket]);
    }

    #[test]
    fn refused_connectionstate_response_drops_connection() {
        let mut engine = connected_engine(0);
        // Gateway answers the keepalive with E_CONNECTION_ID — it no longer
        // recognizes the channel (e.g. it expired during an outage).
        engine.handle_datagram(&connectionstate_response(7, 0x21), 50);
        assert!(!engine.is_connected());
        assert_eq!(drain(&mut engine), vec![Action::ResetSocket]);
        // Reconnect after the backoff window.
        engine.poll(50 + CFG.reconnect_backoff_ms);
        let actions = drain(&mut engine);
        assert!(
            matches!(&actions[..], [Action::Send { frame: f, .. }] if service_type_of(f) == 0x0205)
        );
    }

    #[test]
    fn failed_send_stops_ack_tracking() {
        let addr: GroupAddress = "1/0/8".parse().unwrap();
        let mut engine = connected_engine(0);
        let mut data = heapless::Vec::new();
        data.push(0x01).unwrap();
        engine.handle_command(
            GroupWrite {
                group_addr: addr,
                data,
            },
            10,
        );
        let actions = drain(&mut engine);
        let Some(Action::Send {
            await_ack: Some(seq),
            ..
        }) = actions.last()
        else {
            panic!("expected tracked TUNNELING_REQUEST send, got {actions:?}");
        };

        // The transport reports the send failed: tracking stops, so the sweep
        // does not misreport the local failure as a gateway ACK loss.
        engine.send_failed(*seq);
        engine.poll(10 + CFG.ack_timeout_ms + CFG.ack_sweep_ms + 1);
        assert!(drain(&mut engine)
            .iter()
            .all(|a| !matches!(a, Action::AckTimeout { .. })));
    }

    #[test]
    fn pending_ack_overflow_evicts_and_reports_oldest() {
        let addr: GroupAddress = "1/0/8".parse().unwrap();
        let mut engine = connected_engine(0);
        let mut data = heapless::Vec::new();
        data.push(0x01).unwrap();
        // One more tracked send than the map holds.
        for _ in 0..=PENDING_ACK_CAPACITY {
            engine.handle_command(
                GroupWrite {
                    group_addr: addr,
                    data: data.clone(),
                },
                10,
            );
        }
        let actions = drain(&mut engine);
        // The overflowing send evicted the oldest entry and reported it.
        assert_eq!(
            actions
                .iter()
                .filter(|a| matches!(a, Action::AckTimeout { seq: 0 }))
                .count(),
            1
        );
        // Every send still went out.
        assert_eq!(
            actions
                .iter()
                .filter(|a| matches!(a, Action::Send { .. }))
                .count(),
            PENDING_ACK_CAPACITY + 1
        );
    }

    #[test]
    fn explicit_local_endpoint_is_advertised_in_connect_request() {
        let mut engine = TunnelEngine::new(CFG.clone(), 0);
        engine.set_local_endpoint(LocalEndpoint::Explicit {
            ip: [192, 168, 1, 50],
            port: 54321,
        });
        engine.poll(0);
        let actions = drain(&mut engine);
        let Action::Send { frame: req, .. } = &actions[0] else {
            panic!("expected CONNECT_REQUEST");
        };
        // Control HPAI starts at offset 6: [len, proto, ip(4), port(2)].
        assert_eq!(&req[8..12], &[192, 168, 1, 50]);
        assert_eq!(u16::from_be_bytes([req[12], req[13]]), 54321);
    }
}
