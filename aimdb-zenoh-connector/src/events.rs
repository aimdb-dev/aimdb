//! Typed operational reporting; payloads live only for the synchronous call.

use aimdb_core::LogDisplay;

/// `tracing`/`log` targets: the module that raises each event, as before
/// logging moved here, so per-module target filters keep matching.
/// `tests/log_targets.rs` in aimdb-core checks each names a real module.
#[allow(dead_code)] // some arms are feature-gated
mod target {
    pub(super) const NATIVE: &str = "aimdb_zenoh_connector::native";
    pub(super) const ROS2: &str = "aimdb_zenoh_connector::ros2";
    pub(super) const SHARED: &str = "aimdb_zenoh_connector::shared";
}

#[derive(Debug)]
pub(crate) enum ZenohEvent<'a> {
    PublisherDeclarationFailed {
        error: &'a zenoh::Error,
    },
    InvalidPublishKey {
        topic: &'a str,
    },
    PublishFailed {
        route: &'a str,
        error: &'a zenoh::Error,
    },
    OutboundClosed,
    SessionOpenFailed {
        retry_after: core::time::Duration,
        reason: &'a str,
    },
    SessionOpened {
        session: zenoh::session::ZenohId,
    },
    IngestPanicked {
        topic: &'a str,
    },
    WildcardSampleDropped {
        key: &'a str,
    },
    SubscriberDeclarationFailed {
        error: &'a zenoh::Error,
    },
    Ros2CodecUnchecked {
        topic: &'a str,
    },
    Ros2DomainWarning {
        domain: u32,
    },
    Ros2LivelinessDeclarationFailed {
        error: &'a zenoh::Error,
    },
    Ros2SubscriberDeclarationFailed {
        error: &'a zenoh::Error,
    },
    Ros2PublisherDeclarationFailed {
        topic: &'a str,
        error: &'a zenoh::Error,
    },
    Ros2PublishFailed {
        topic: &'a str,
        error: &'a zenoh::Error,
    },
    Ros2RegistrationMissing,
}

pub(crate) fn report(event: ZenohEvent<'_>) {
    #[cfg(test)]
    capture::record(&event);
    match event {
        ZenohEvent::PublisherDeclarationFailed { error } => {
            aimdb_core::log_error!(
                target: target::NATIVE,
                "Zenoh: cannot declare a publisher: {}",
                LogDisplay(error)
            )
        }
        ZenohEvent::InvalidPublishKey { topic } => {
            aimdb_core::log_error!(
                target: target::NATIVE,
                "Zenoh: '{}' is not a key without wildcards",
                topic
            )
        }
        ZenohEvent::PublishFailed { route, error } => {
            aimdb_core::log_error!(
                target: target::NATIVE,
                "Zenoh: put on route '{}' failed: {}",
                route,
                LogDisplay(error)
            )
        }
        ZenohEvent::OutboundClosed => {
            aimdb_core::log_info!(target: target::NATIVE, "Zenoh: every outbound route has closed")
        }
        ZenohEvent::SessionOpenFailed {
            retry_after,
            reason,
        } => {
            aimdb_core::log_warn!(
                target: target::NATIVE,
                "Zenoh: cannot open the session, retrying in {:?}: {}",
                retry_after,
                reason
            );
        }
        ZenohEvent::SessionOpened { session } => {
            aimdb_core::log_info!(
                target: target::NATIVE,
                "Zenoh: session {} open",
                LogDisplay(&session)
            )
        }
        ZenohEvent::IngestPanicked { topic } => {
            aimdb_core::log_error!(
                target: target::NATIVE,
                "Zenoh: ingest panicked on '{}'; sample dropped",
                topic
            )
        }
        ZenohEvent::WildcardSampleDropped { key } => {
            aimdb_core::log_warn!(
                target: target::NATIVE,
                "Zenoh: dropping samples on wildcard keys, first '{}'",
                key
            )
        }
        ZenohEvent::SubscriberDeclarationFailed { error } => {
            aimdb_core::log_error!(
                target: target::NATIVE,
                "Zenoh: cannot declare a subscriber: {}",
                LogDisplay(error)
            )
        }
        ZenohEvent::Ros2CodecUnchecked { topic } => {
            aimdb_core::log_warn!(
                target: target::ROS2,
                "ros2://{}: a custom serializer or deserializer is installed; nothing checks that it speaks CDR",
                topic.strip_prefix('/').unwrap_or(topic)
            );
        }
        ZenohEvent::Ros2DomainWarning { domain } => {
            aimdb_core::log_warn!(
                target: target::ROS2,
                "ROS 2 domain {} is above {}, the highest ROS 2 documents; rmw_zenoh accepts it, DDS-based ROS 2 does not",
                domain,
                crate::ros2::DOMAIN_MAX
            );
        }
        ZenohEvent::Ros2LivelinessDeclarationFailed { error } => {
            aimdb_core::log_error!(
                target: target::ROS2,
                "ROS 2: cannot declare a liveliness token: {}",
                LogDisplay(error)
            )
        }
        ZenohEvent::Ros2SubscriberDeclarationFailed { error } => {
            aimdb_core::log_error!(
                target: target::ROS2,
                "ROS 2: cannot declare a subscriber: {}",
                LogDisplay(error)
            )
        }
        ZenohEvent::Ros2PublisherDeclarationFailed { topic, error } => {
            aimdb_core::log_error!(
                target: target::ROS2,
                "ROS 2: cannot declare a publisher on '{}': {}",
                topic,
                LogDisplay(error)
            );
        }
        ZenohEvent::Ros2PublishFailed { topic, error } => {
            aimdb_core::log_error!(
                target: target::ROS2,
                "ROS 2: put on '{}' failed: {}",
                topic,
                LogDisplay(error)
            )
        }
        ZenohEvent::Ros2RegistrationMissing => {
            aimdb_core::log_warn!(
                target: target::SHARED,
                "Zenoh: a Ros2Connector on this session was never registered"
            )
        }
    }
}

#[cfg(test)]
aimdb_core::__event_capture!(super::ZenohEvent<'_>);
