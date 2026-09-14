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

/// Session cadence: how often to ping, how long to wait, when to give up.
#[derive(Debug, Clone, Copy)]
pub struct Settings {
    /// Minimum interval between pings.
    pub ping_interval: Duration,
    /// Maximum silence from the broker before the session is declared dead.
    pub connection_event_max_interval: Duration,
    /// Wait between a failed session and the next dial.
    pub reconnection_delay: Duration,
    /// Maximum round-trip wait for a packet that expects a response.
    pub response_timeout: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ping_interval: Duration::from_millis(2_000),
            connection_event_max_interval: Duration::from_millis(10_000),
            reconnection_delay: Duration::from_millis(2_000),
            response_timeout: Duration::from_millis(5_000),
        }
    }
}
