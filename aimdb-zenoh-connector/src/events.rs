//! Typed operational reporting; payloads live only for the synchronous call.

/// Borrowed foreign diagnostic, retaining the original Display/Debug policy.
pub(crate) enum Detail<'a> {
    Display(&'a dyn core::fmt::Display),
}
impl<'a> Detail<'a> {
    pub(crate) fn display(value: &'a impl core::fmt::Display) -> Self {
        Self::Display(value)
    }
}
impl core::fmt::Display for Detail<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Display(value) => core::fmt::Display::fmt(value, f),
        }
    }
}
impl core::fmt::Debug for Detail<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(self, f)
    }
}
#[cfg(feature = "defmt")]
impl defmt::Format for Detail<'_> {
    fn format(&self, f: defmt::Formatter) {
        defmt::write!(f, "{}", defmt::Display2Format(self));
    }
}

#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) enum ZenohEvent<'a> {
    PublisherDeclarationFailed {
        error: Detail<'a>,
    },
    InvalidPublishKey {
        topic: &'a str,
    },
    PublishFailed {
        route: &'a str,
        error: Detail<'a>,
    },
    OutboundClosed,
    SessionOpenFailed {
        retry_after: core::time::Duration,
        reason: &'a str,
    },
    SessionOpened {
        session: Detail<'a>,
    },
    IngestPanicked {
        topic: &'a str,
    },
    WildcardSampleDropped {
        key: &'a str,
    },
    SubscriberDeclarationFailed {
        error: Detail<'a>,
    },
    Ros2CodecUnchecked {
        topic: &'a str,
    },
    Ros2DomainWarning {
        domain: u32,
    },
    Ros2LivelinessDeclarationFailed {
        error: Detail<'a>,
    },
    Ros2SubscriberDeclarationFailed {
        error: Detail<'a>,
    },
    Ros2PublisherDeclarationFailed {
        topic: &'a str,
        error: Detail<'a>,
    },
    Ros2PublishFailed {
        topic: &'a str,
        error: Detail<'a>,
    },
    Ros2RegistrationMissing,
}

pub(crate) fn report(event: ZenohEvent<'_>) {
    #[cfg(test)]
    capture::record(&event);
    match event {
        ZenohEvent::PublisherDeclarationFailed { error } => {
            aimdb_core::log_error!("Zenoh: cannot declare a publisher: {}", error)
        }
        ZenohEvent::InvalidPublishKey { topic } => {
            aimdb_core::log_error!("Zenoh: '{}' is not a key without wildcards", topic)
        }
        ZenohEvent::PublishFailed { route, error } => {
            aimdb_core::log_error!("Zenoh: put on route '{}' failed: {}", route, error)
        }
        ZenohEvent::OutboundClosed => {
            aimdb_core::log_info!("Zenoh: every outbound route has closed")
        }
        ZenohEvent::SessionOpenFailed {
            retry_after,
            reason,
        } => aimdb_core::log_warn!(
            "Zenoh: cannot open the session, retrying in {:?}: {}",
            retry_after,
            reason
        ),
        ZenohEvent::SessionOpened { session } => {
            aimdb_core::log_info!("Zenoh: session {} open", session)
        }
        ZenohEvent::IngestPanicked { topic } => {
            aimdb_core::log_error!("Zenoh: ingest panicked on '{}'; sample dropped", topic)
        }
        ZenohEvent::WildcardSampleDropped { key } => {
            aimdb_core::log_warn!("Zenoh: dropping samples on wildcard keys, first '{}'", key)
        }
        ZenohEvent::SubscriberDeclarationFailed { error } => {
            aimdb_core::log_error!("Zenoh: cannot declare a subscriber: {}", error)
        }
        ZenohEvent::Ros2CodecUnchecked { topic } => aimdb_core::log_warn!("ros2://{}: a custom serializer or deserializer is installed; nothing checks that it speaks CDR", topic.strip_prefix('/').unwrap_or(topic)),
        ZenohEvent::Ros2DomainWarning { domain } => aimdb_core::log_warn!(
            "ROS 2 domain {} is above {}, the highest ROS 2 documents; rmw_zenoh accepts it, DDS-based ROS 2 does not",
            domain, crate::ros2::DOMAIN_MAX
        ),
        ZenohEvent::Ros2LivelinessDeclarationFailed { error } => {
            aimdb_core::log_error!("ROS 2: cannot declare a liveliness token: {}", error)
        }
        ZenohEvent::Ros2SubscriberDeclarationFailed { error } => {
            aimdb_core::log_error!("ROS 2: cannot declare a subscriber: {}", error)
        }
        ZenohEvent::Ros2PublisherDeclarationFailed { topic, error } => aimdb_core::log_error!(
            "ROS 2: cannot declare a publisher on '{}': {}",
            topic,
            error
        ),
        ZenohEvent::Ros2PublishFailed { topic, error } => {
            aimdb_core::log_error!("ROS 2: put on '{}' failed: {}", topic, error)
        }
        ZenohEvent::Ros2RegistrationMissing => {
            aimdb_core::log_warn!("Zenoh: a Ros2Connector on this session was never registered")
        }
    }
}

/// Test-only poll-scoped observation. Spawned work must be wrapped explicitly.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) mod capture {
    extern crate std;
    use super::ZenohEvent;
    use core::future::{poll_fn, Future};
    use std::{boxed::Box, cell::RefCell, sync::Arc};
    pub(crate) type Observer = Arc<dyn Fn(&ZenohEvent<'_>) + Send + Sync>;
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
    pub(super) fn record(event: &ZenohEvent<'_>) {
        let observer = CURRENT.with(|slot| slot.borrow().clone());
        if let Some(observer) = observer {
            observer(event);
        }
    }
}
