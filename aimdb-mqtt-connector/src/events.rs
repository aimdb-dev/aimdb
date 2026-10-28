//! Typed operational reporting; payloads live only for the synchronous call.

/// Borrowed foreign diagnostic, retaining the original Display/Debug policy.
#[cfg(any(feature = "std", feature = "embedded-tls"))]
pub(crate) enum Detail<'a> {
    #[cfg(feature = "std")]
    Display(&'a dyn core::fmt::Display),
    Debug(&'a dyn core::fmt::Debug),
}
#[cfg(any(feature = "std", feature = "embedded-tls"))]
impl<'a> Detail<'a> {
    #[cfg(feature = "std")]
    pub(crate) fn display(value: &'a impl core::fmt::Display) -> Self {
        Self::Display(value)
    }
    pub(crate) fn debug(value: &'a impl core::fmt::Debug) -> Self {
        Self::Debug(value)
    }
}
#[cfg(any(feature = "std", feature = "embedded-tls"))]
impl core::fmt::Display for Detail<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            #[cfg(feature = "std")]
            Self::Display(value) => core::fmt::Display::fmt(value, f),
            Self::Debug(value) => core::fmt::Debug::fmt(value, f),
        }
    }
}
#[cfg(any(feature = "std", feature = "embedded-tls"))]
impl core::fmt::Debug for Detail<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(self, f)
    }
}
#[cfg(all(feature = "defmt", any(feature = "std", feature = "embedded-tls")))]
impl defmt::Format for Detail<'_> {
    fn format(&self, f: defmt::Formatter) {
        defmt::write!(f, "{}", defmt::Display2Format(self));
    }
}

#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
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
    TlsHandshakeFailed { error: Detail<'a> },
    #[cfg(feature = "embedded-tls")]
    TlsEstablished,
    #[cfg(feature = "embedded-tls")]
    TlsSessionFailed {
        error: crate::embedded::manager::Error,
    },
    #[cfg(feature = "std")]
    NativeEventLoopFailed { broker: &'a str, error: Detail<'a> },
    #[cfg(feature = "std")]
    NativePublishFailed { route: &'a str, error: Detail<'a> },
    #[cfg(feature = "std")]
    OutboundClosed,
}

pub(crate) fn report(event: MqttEvent<'_>) {
    #[cfg(test)]
    capture::record(&event);
    match event {
        #[cfg(feature = "embedded")]
        MqttEvent::SubscribeQosDowngraded { topic } => {
            aimdb_core::log_warn!("MQTT: inbound route '{}' asks for qos=2; this backend subscribes at QoS 1 (at-least-once). The std backend honours qos=2 on the same URL.", topic);
        }
        #[cfg(feature = "embedded")]
        MqttEvent::PublishQosDowngraded { topic } => {
            aimdb_core::log_warn!("MQTT: route '{}' asks for qos=2; this backend publishes it at QoS 1 (at-least-once). The std backend honours qos=2 on the same URL.", topic);
        }
        #[cfg(feature = "embedded")]
        MqttEvent::BuildFailed { reason } => {
            aimdb_core::log_error!("Failed to build MQTT connector: {}", reason);
        }
        #[cfg(feature = "embedded")]
        MqttEvent::BackgroundStarting => {
            aimdb_core::log_info!("MQTT background task starting");
        }
        #[cfg(feature = "embedded-tls")]
        MqttEvent::TlsIpLiteral => {
            aimdb_core::log_warn!("MQTT-TLS: broker host is an IP literal; the certificate must carry it in CN — prefer a hostname");
        }
        #[cfg(feature = "embedded-tls")]
        MqttEvent::TlsBackgroundStarting => {
            aimdb_core::log_info!("MQTT-TLS background task starting");
        }
        #[cfg(feature = "embedded")]
        MqttEvent::ConnectFailed => {
            aimdb_core::log_warn!("MQTT: connect failed, will retry");
        }
        #[cfg(feature = "embedded")]
        MqttEvent::SessionFailed { error } => {
            aimdb_core::log_warn!("MQTT: session errored: {:?}", error);
        }
        #[cfg(feature = "embedded")]
        MqttEvent::BrokerUnresponsive => {
            aimdb_core::log_warn!("MQTT: broker unresponsive");
        }
        #[cfg(feature = "embedded")]
        MqttEvent::PingDropped => {
            aimdb_core::log_warn!("MQTT: write ring full, ping dropped");
        }
        #[cfg(feature = "embedded")]
        MqttEvent::PublishTooLarge {
            topic,
            bytes,
            broker_limit,
        } => {
            aimdb_core::log_warn!(
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
            aimdb_core::log_info!("SNTP: synced, unix time {}", unix_seconds);
        }
        #[cfg(feature = "embassy-tls")]
        MqttEvent::ClockImplausible {
            unix_seconds,
            server,
        } => {
            aimdb_core::log_warn!(
                "SNTP: implausible time {} from {}, retrying",
                unix_seconds,
                server
            );
        }
        #[cfg(feature = "embassy-tls")]
        MqttEvent::ClockSyncFailed { server, error } => {
            aimdb_core::log_warn!("SNTP: sync with {} failed: {}, retrying", server, error);
        }
        #[cfg(feature = "embedded-tls")]
        MqttEvent::TlsWaitingForClock => {
            aimdb_core::log_info!("MQTT-TLS: waiting for a wall-clock reading...");
        }
        #[cfg(feature = "embedded-tls")]
        MqttEvent::TlsConnectFailed => {
            aimdb_core::log_warn!("MQTT-TLS: connect failed, will retry");
        }
        #[cfg(feature = "embedded-tls")]
        MqttEvent::TlsHandshakeFailed { error } => {
            aimdb_core::log_warn!("MQTT-TLS: handshake failed, will retry: {:?}", error);
        }
        #[cfg(feature = "embedded-tls")]
        MqttEvent::TlsEstablished => {
            aimdb_core::log_info!("MQTT-TLS: session established");
        }
        #[cfg(feature = "embedded-tls")]
        MqttEvent::TlsSessionFailed { error } => {
            aimdb_core::log_warn!("MQTT-TLS: session errored: {:?}", error);
        }
        #[cfg(feature = "std")]
        MqttEvent::NativeEventLoopFailed { broker, error } => {
            aimdb_core::log_error!("MQTT event loop error for {}: {:?}", broker, error);
        }
        #[cfg(feature = "std")]
        MqttEvent::NativePublishFailed { route, error } => {
            aimdb_core::log_error!("MQTT publish on route '{}' failed: {}", route, error);
        }
        #[cfg(feature = "std")]
        MqttEvent::OutboundClosed => {
            aimdb_core::log_info!("MQTT publish loop: every outbound route has closed");
        }
    }
}

/// Test-only poll-scoped observation. Spawned work must be wrapped explicitly.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) mod capture {
    extern crate std;
    use super::MqttEvent;
    use core::future::{poll_fn, Future};
    use std::{boxed::Box, cell::RefCell, sync::Arc};
    pub(crate) type Observer = Arc<dyn Fn(&MqttEvent<'_>) + Send + Sync>;
    std::thread_local! {
        static CURRENT: RefCell<Option<Observer>> = const { RefCell::new(None) };
    }
    struct Guard(Option<Observer>);
    impl Drop for Guard {
        fn drop(&mut self) {
            CURRENT.with(|slot| {
                slot.replace(self.0.take());
            });
        }
    }
    pub(crate) fn with<T>(observer: Observer, f: impl FnOnce() -> T) -> T {
        let _guard = Guard(CURRENT.with(|slot| slot.replace(Some(observer))));
        f()
    }
    pub(crate) async fn future<F: Future>(observer: Observer, future: F) -> F::Output {
        let mut future = Box::pin(future);
        poll_fn(|cx| with(observer.clone(), || future.as_mut().poll(cx))).await
    }
    pub(super) fn record(event: &MqttEvent<'_>) {
        let observer = CURRENT.with(|slot| slot.borrow().clone());
        if let Some(observer) = observer {
            observer(event);
        }
    }
}
