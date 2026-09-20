//! Session cadence and the channels a session talks over.
//!
//! Channels use `CriticalSectionRawMutex`, so they are `Sync` and the sink and
//! source need no force-`Send` wrapper. Time comes from core's
//! [`aimdb_core::session::Delay`], so nothing here names an executor.

use core::time::Duration;

use aimdb_core::RuntimeOps;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use mountain_mqtt::client::{ClientError, EventHandlerError};
use mountain_mqtt::packets::publish::ApplicationMessage;

/// The event channel: broker session to `pump_source`.
pub(crate) type EventChannel<E, const Q: usize> = Channel<CriticalSectionRawMutex, E, Q>;

/// The action channel: `pump_sink` to broker session.
pub(crate) type ActionChannel<A, const Q: usize> = Channel<CriticalSectionRawMutex, A, Q>;

/// Monotonic milliseconds. Only differences are meaningful.
pub(crate) fn now_ms(runtime: &dyn RuntimeOps) -> u64 {
    runtime.now_nanos() / 1_000_000
}

/// Convert a received [`ApplicationMessage`] into an application event.
pub trait FromApplicationMessage<const P: usize>: Sized {
    /// Build the event, or reject the message.
    fn from_application_message(message: &ApplicationMessage<P>)
        -> Result<Self, EventHandlerError>;
}

/// Why a session ended.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Error {
    /// The MQTT client reported an error.
    Client(ClientError),
    /// No acknowledgement arrived within `connection_event_max_interval`.
    MqttServerUnresponsive,
}

impl From<ClientError> for Error {
    fn from(value: ClientError) -> Self {
        Self::Client(value)
    }
}

#[cfg(feature = "defmt")]
impl defmt::Format for Error {
    fn format(&self, f: defmt::Formatter) {
        match self {
            Error::Client(e) => defmt::write!(f, "Client({})", e),
            Error::MqttServerUnresponsive => defmt::write!(f, "MqttServerUnresponsive"),
        }
    }
}

/// Session cadence, derived from the keep-alive the CONNECT promises.
#[derive(Debug, Clone, Copy)]
pub struct Settings {
    /// What the CONNECT promises the broker, in seconds — its own unit.
    pub keep_alive_secs: u16,
    /// Minimum interval between pings. Half the keep-alive, so a lost ping
    /// still leaves a whole interval before anyone gives up.
    pub ping_interval: Duration,
    /// Maximum silence from the broker before the session is declared dead.
    /// One and a half keep-alives, mirroring the rule MQTT gives the *broker*
    /// for disconnecting a silent client, so the two sides give up together.
    pub connection_event_max_interval: Duration,
    /// Wait between a failed session and the next dial.
    pub reconnection_delay: Duration,
    /// Maximum round-trip wait for a packet that expects one — CONNACK, SUBACK,
    /// or the PUBACK of a QoS 1 publish. A whole keep-alive, which puts it
    /// between the ping interval and the liveness window: a ping is never
    /// racing an outstanding acknowledgement, and the acknowledgement always
    /// gives up before the session does.
    pub response_timeout: Duration,
}

impl Settings {
    /// Derive the cadence from a keep-alive in seconds.
    ///
    /// The caller has already rejected values too small to halve — see
    /// `MqttConnector::with_keep_alive`.
    pub(crate) fn from_keep_alive_secs(keep_alive_secs: u16) -> Self {
        let keep_alive = Duration::from_secs(u64::from(keep_alive_secs));
        Self {
            keep_alive_secs,
            ping_interval: keep_alive / 2,
            connection_event_max_interval: keep_alive * 3 / 2,
            // Backoff between dials, not a cadence: nothing about the
            // keep-alive says how long to wait before trying again.
            reconnection_delay: Duration::from_millis(2_000),
            response_timeout: keep_alive,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self::from_keep_alive_secs(crate::connector::KEEP_ALIVE_DEFAULT_SECS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three numbers move together, and in the order the protocol needs.
    #[test]
    fn the_cadence_is_derived_from_the_promise() {
        for secs in [10u16, 45, 60, 300, u16::MAX] {
            let s = Settings::from_keep_alive_secs(secs);
            let keep_alive = Duration::from_secs(u64::from(secs));

            assert_eq!(s.keep_alive_secs, secs, "the promise is sent verbatim");
            assert_eq!(s.ping_interval, keep_alive / 2);
            assert_eq!(s.connection_event_max_interval, keep_alive * 3 / 2);

            // What the derivation exists to guarantee: we always speak well
            // before the broker may hang up, and we never declare a session
            // dead before a ping has had a full interval to be answered.
            assert!(
                s.ping_interval < keep_alive,
                "a ping must land inside the keep-alive it promised"
            );
            assert!(
                s.connection_event_max_interval > s.ping_interval * 2,
                "one lost ping must not be enough to abandon the session"
            );
            // Strictly ordered, so no two deadlines can come due together: a
            // ping never races an outstanding acknowledgement, and that
            // acknowledgement gives up before the whole session does.
            assert!(
                s.ping_interval < s.response_timeout
                    && s.response_timeout < s.connection_event_max_interval,
                "ping < response < liveness must hold at every keep-alive"
            );
        }
    }

    /// The default is the same 60 s the CONNECT used to carry by accident.
    #[test]
    fn the_default_promises_sixty_seconds() {
        let s = Settings::default();
        assert_eq!(s.keep_alive_secs, 60);
        assert_eq!(s.ping_interval, Duration::from_secs(30));
        assert_eq!(s.response_timeout, Duration::from_secs(60));
        assert_eq!(s.connection_event_max_interval, Duration::from_secs(90));
    }
}
