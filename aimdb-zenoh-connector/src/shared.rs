//! One Zenoh session behind a `ZenohConnector` and the `Ros2Connector`
//! derived from it.
//!
//! Each connector's `build()` validates its links and leaves its parts here.
//! The first `build()` to run returns the session task; the other returns
//! none. `AimDbBuilder` runs every `build()` before it polls any future, so
//! the task finds every part when it first runs.

use std::boxed::Box;
use std::string::String;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::vec::Vec;

use aimdb_core::{log_warn, DbResult};

use crate::connector::BoxFuture;
use crate::native::{open, session_config, ZenohParts};
use crate::ros2::Ros2Parts;

/// The session the views share, and what each view left for it.
pub(crate) struct Shared {
    endpoint: String,
    config: Mutex<Option<zenoh::Config>>,
    zenoh: Mutex<Option<ZenohParts>>,
    ros2: Mutex<Option<Ros2Parts>>,
    /// A `build()` has returned the session task.
    task_taken: AtomicBool,
    /// A `Ros2Connector` exists on this session.
    ros2_view: AtomicBool,
}

impl Shared {
    pub(crate) fn new(endpoint: String) -> Arc<Self> {
        Arc::new(Self {
            endpoint,
            config: Mutex::new(None),
            zenoh: Mutex::new(None),
            ros2: Mutex::new(None),
            task_taken: AtomicBool::new(false),
            ros2_view: AtomicBool::new(false),
        })
    }

    pub(crate) fn set_config(&self, config: zenoh::Config) {
        *self.config.lock().unwrap_or_else(PoisonError::into_inner) = Some(config);
    }

    pub(crate) fn ros2_view_created(&self) {
        self.ros2_view.store(true, Ordering::Relaxed);
    }

    pub(crate) fn deposit_zenoh(self: &Arc<Self>, parts: ZenohParts) -> DbResult<Vec<BoxFuture>> {
        *self.zenoh.lock().unwrap_or_else(PoisonError::into_inner) = Some(parts);
        self.task()
    }

    pub(crate) fn deposit_ros2(self: &Arc<Self>, parts: Ros2Parts) -> DbResult<Vec<BoxFuture>> {
        *self.ros2.lock().unwrap_or_else(PoisonError::into_inner) = Some(parts);
        self.task()
    }

    /// The session task for the first caller; nothing for the next.
    fn task(self: &Arc<Self>) -> DbResult<Vec<BoxFuture>> {
        let config = self
            .config
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        // Validated by every view, so a bad endpoint fails whichever builds.
        let config = session_config(&self.endpoint, config)?;
        if self.task_taken.swap(true, Ordering::AcqRel) {
            return Ok(Vec::new());
        }
        let shared = self.clone();
        let task: BoxFuture = Box::pin(async move { shared.run(config).await });
        Ok(Vec::from([task]))
    }

    async fn run(self: Arc<Self>, config: zenoh::Config) {
        let zenoh = self
            .zenoh
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let ros2 = self
            .ros2
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if ros2.is_none() && self.ros2_view.load(Ordering::Relaxed) {
            log_warn!("Zenoh: a Ros2Connector on this session was never registered");
        }
        let session = open(config).await;
        let zenoh = async {
            match zenoh {
                Some(parts) => parts.serve(&session).await,
                None => core::future::pending().await,
            }
        };
        let ros2 = async {
            match ros2 {
                Some(parts) => parts.serve(&session).await,
                None => core::future::pending().await,
            }
        };
        tokio::join!(zenoh, ros2);
    }
}
