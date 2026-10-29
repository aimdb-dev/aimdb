//! Typed operational reporting; payloads live only for the synchronous call.

/// `tracing`/`log` targets: the module that raises each event, as before
/// logging moved here, so per-module target filters keep matching.
/// `tests/log_targets.rs` in aimdb-core checks each names a real module.
#[allow(dead_code)] // some arms are feature-gated
mod target {
    pub(super) const EMBEDDED: &str = "aimdb_mqtt_connector::embedded";
    pub(super) const NATIVE: &str = "aimdb_mqtt_connector::native";
    pub(super) const SESSION: &str = "aimdb_mqtt_connector::embedded::session";
    pub(super) const SESSION_LOOP: &str = "aimdb_mqtt_connector::embedded::session_loop";
    pub(super) const SNTP: &str = "aimdb_mqtt_connector::embedded::sntp";
    pub(super) const TLS: &str = "aimdb_mqtt_connector::embedded::tls";
}

#[cfg(any(feature = "std", feature = "embedded-tls"))]
use aimdb_core::LogDebug;
#[cfg(feature = "std")]
use aimdb_core::LogDisplay;

#[derive(Debug)]
pub(crate) enum MqttEvent<'a> {
    #[cfg(feature = "embedded")]
    SubscribeQosDowngraded { topic: &'a str },
    #[cfg(feature = "embedded")]
    PublishQosDowngraded { topic: &'a str },
    #[cfg(feature = "embedded")]
    BuildFailed { reason: &'a str },
    #[cfg(feature = "embedded")]
    BackgroundStarting,
    #[cfg(feature = "embedded-tls")]
    TlsIpLiteral,
    #[cfg(feature = "embedded-tls")]
    TlsBackgroundStarting,
    #[cfg(feature = "embedded")]
    ConnectFailed,
    #[cfg(feature = "embedded")]
    SessionFailed {
        error: crate::embedded::manager::Error,
    },
    #[cfg(feature = "embedded")]
    BrokerUnresponsive,
    #[cfg(feature = "embedded")]
    PingDropped,
    #[cfg(feature = "embedded")]
    PublishTooLarge {
        topic: &'a str,
        bytes: usize,
        broker_limit: bool,
    },
    #[cfg(feature = "embassy-tls")]
    ClockSynced { unix_seconds: u64 },
    #[cfg(feature = "embassy-tls")]
    ClockImplausible { unix_seconds: u64, server: &'a str },
    #[cfg(feature = "embassy-tls")]
    ClockSyncFailed {
        server: &'a str,
        error: crate::embedded::sntp::SntpError,
    },
    #[cfg(feature = "embedded-tls")]
    TlsWaitingForClock,
    #[cfg(feature = "embedded-tls")]
    TlsConnectFailed,
    #[cfg(feature = "embedded-tls")]
    TlsHandshakeFailed { error: embedded_tls::TlsError },
    #[cfg(feature = "embedded-tls")]
    TlsEstablished,
    #[cfg(feature = "embedded-tls")]
    TlsSessionFailed {
        error: crate::embedded::manager::Error,
    },
    #[cfg(feature = "std")]
    NativeEventLoopFailed {
        broker: &'a str,
        error: &'a rumqttc::ConnectionError,
    },
    #[cfg(feature = "std")]
    NativePublishFailed {
        route: &'a str,
        error: &'a rumqttc::ClientError,
    },
    #[cfg(feature = "std")]
    OutboundClosed,
}

pub(crate) fn report(event: MqttEvent<'_>) {
    #[cfg(test)]
    capture::record(&event);
    match event {
        #[cfg(feature = "embedded")]
        MqttEvent::SubscribeQosDowngraded { topic } => {
            aimdb_core::log_warn!(
                target: target::EMBEDDED,
                "MQTT: inbound route '{}' asks for qos=2; this backend subscribes at QoS 1 (at-least-once). The std backend honours qos=2 on the same URL.",
                topic
            );
        }
        #[cfg(feature = "embedded")]
        MqttEvent::PublishQosDowngraded { topic } => {
            aimdb_core::log_warn!(
                target: target::EMBEDDED,
                "MQTT: route '{}' asks for qos=2; this backend publishes it at QoS 1 (at-least-once). The std backend honours qos=2 on the same URL.",
                topic
            );
        }
        #[cfg(feature = "embedded")]
        MqttEvent::BuildFailed { reason } => {
            aimdb_core::log_error!(
                target: target::EMBEDDED,
                "Failed to build MQTT connector: {}",
                reason
            );
        }
        #[cfg(feature = "embedded")]
        MqttEvent::BackgroundStarting => {
            aimdb_core::log_info!(target: target::EMBEDDED, "MQTT background task starting");
        }
        #[cfg(feature = "embedded-tls")]
        MqttEvent::TlsIpLiteral => {
            aimdb_core::log_warn!(
                target: target::EMBEDDED,
                "MQTT-TLS: broker host is an IP literal; the certificate must carry it in CN — prefer a hostname"
            );
        }
        #[cfg(feature = "embedded-tls")]
        MqttEvent::TlsBackgroundStarting => {
            aimdb_core::log_info!(target: target::EMBEDDED, "MQTT-TLS background task starting");
        }
        #[cfg(feature = "embedded")]
        MqttEvent::ConnectFailed => {
            aimdb_core::log_warn!(target: target::SESSION, "MQTT: connect failed, will retry");
        }
        #[cfg(feature = "embedded")]
        MqttEvent::SessionFailed { error } => {
            aimdb_core::log_warn!(target: target::SESSION, "MQTT: session errored: {:?}", error);
        }
        #[cfg(feature = "embedded")]
        MqttEvent::BrokerUnresponsive => {
            aimdb_core::log_warn!(target: target::SESSION_LOOP, "MQTT: broker unresponsive");
        }
        #[cfg(feature = "embedded")]
        MqttEvent::PingDropped => {
            aimdb_core::log_warn!(
                target: target::SESSION_LOOP,
                "MQTT: write ring full, ping dropped"
            );
        }
        #[cfg(feature = "embedded")]
        MqttEvent::PublishTooLarge {
            topic,
            bytes,
            broker_limit,
        } => {
            aimdb_core::log_warn!(
                target: target::SESSION_LOOP,
                "MQTT: skipping publish to '{}': {} bytes exceed the {} limit",
                topic,
                bytes,
                if broker_limit {
                    "broker's"
                } else {
                    "write ring's"
                }
            );
        }
        #[cfg(feature = "embassy-tls")]
        MqttEvent::ClockSynced { unix_seconds } => {
            aimdb_core::log_info!(target: target::SNTP, "SNTP: synced, unix time {}", unix_seconds);
        }
        #[cfg(feature = "embassy-tls")]
        MqttEvent::ClockImplausible {
            unix_seconds,
            server,
        } => {
            aimdb_core::log_warn!(
                target: target::SNTP,
                "SNTP: implausible time {} from {}, retrying",
                unix_seconds,
                server
            );
        }
        #[cfg(feature = "embassy-tls")]
        MqttEvent::ClockSyncFailed { server, error } => {
            aimdb_core::log_warn!(
                target: target::SNTP,
                "SNTP: sync with {} failed: {}, retrying",
                server,
                error
            );
        }
        #[cfg(feature = "embedded-tls")]
        MqttEvent::TlsWaitingForClock => {
            aimdb_core::log_info!(
                target: target::TLS,
                "MQTT-TLS: waiting for a wall-clock reading..."
            );
        }
        #[cfg(feature = "embedded-tls")]
        MqttEvent::TlsConnectFailed => {
            aimdb_core::log_warn!(target: target::TLS, "MQTT-TLS: connect failed, will retry");
        }
        #[cfg(feature = "embedded-tls")]
        MqttEvent::TlsHandshakeFailed { error } => {
            aimdb_core::log_warn!(
                target: target::TLS,
                "MQTT-TLS: handshake failed, will retry: {:?}",
                LogDebug(&error)
            );
        }
        #[cfg(feature = "embedded-tls")]
        MqttEvent::TlsEstablished => {
            aimdb_core::log_info!(target: target::TLS, "MQTT-TLS: session established");
        }
        #[cfg(feature = "embedded-tls")]
        MqttEvent::TlsSessionFailed { error } => {
            aimdb_core::log_warn!(target: target::TLS, "MQTT-TLS: session errored: {:?}", error);
        }
        #[cfg(feature = "std")]
        MqttEvent::NativeEventLoopFailed { broker, error } => {
            aimdb_core::log_error!(
                target: target::NATIVE,
                "MQTT event loop error for {}: {:?}",
                broker,
                LogDebug(error)
            );
        }
        #[cfg(feature = "std")]
        MqttEvent::NativePublishFailed { route, error } => {
            aimdb_core::log_error!(
                target: target::NATIVE,
                "MQTT publish on route '{}' failed: {}",
                route,
                LogDisplay(error)
            );
        }
        #[cfg(feature = "std")]
        MqttEvent::OutboundClosed => {
            aimdb_core::log_info!(
                target: target::NATIVE,
                "MQTT publish loop: every outbound route has closed"
            );
        }
    }
}

#[cfg(test)]
aimdb_core::__event_capture!(super::MqttEvent<'_>);
