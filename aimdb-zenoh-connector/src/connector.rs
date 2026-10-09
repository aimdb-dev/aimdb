//! `ZenohConnector` over a protocol backend.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;

use aimdb_core::connector::ConnectorBuilder;
use aimdb_core::{AimDb, DbResult};

/// The runner's collected future type.
pub(crate) type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
/// What [`ConnectorBuilder::build`] returns.
pub(crate) type BuildFuture<'a> =
    Pin<Box<dyn Future<Output = DbResult<Vec<BoxFuture>>> + Send + 'a>>;

/// The `zenoh` crate backend. Selected by supplying no transport.
#[derive(Clone)]
pub struct Native {
    #[cfg(feature = "std")]
    pub(crate) shared: alloc::sync::Arc<crate::shared::Shared>,
}

impl Native {
    #[cfg_attr(not(feature = "std"), allow(unused_variables))]
    fn new(endpoint: &str) -> Self {
        Self {
            #[cfg(feature = "std")]
            shared: crate::shared::Shared::new(String::from(endpoint)),
        }
    }
}

/// A connector for `zenoh://` links over the backend `B`.
///
/// ```rust,ignore
/// let zenoh = ZenohConnector::new("tcp/192.168.10.5:7447");
/// builder.with_connector(zenoh);
/// ```
pub struct ZenohConnector<B = Native> {
    pub(crate) endpoint: String,
    pub(crate) backend: B,
}

impl ZenohConnector<Native> {
    /// Connect to a Zenoh router at `endpoint`, in Zenoh's locator syntax
    /// (`tcp/host:port`). An empty endpoint connects nowhere, for a session
    /// whose `with_zenoh_config` (with `std`) says where to go.
    pub fn new(endpoint: impl Into<String>) -> Self {
        let endpoint = endpoint.into();
        Self {
            backend: Native::new(&endpoint),
            endpoint,
        }
    }

    /// Start from `config` instead of Zenoh's defaults in client mode: peer
    /// mode, TLS, QUIC, access control. The endpoint passed to
    /// [`new`](Self::new), if not empty, replaces `connect/endpoints`.
    #[cfg(feature = "std")]
    pub fn with_zenoh_config(self, config: zenoh::Config) -> Self {
        self.backend.shared.set_config(config);
        self
    }

    /// `ros2://` links on this connector's session: one router connection for
    /// both schemes. Register both connectors.
    ///
    /// ```rust,ignore
    /// let zenoh = ZenohConnector::new("tcp/192.168.10.5:7447");
    /// let ros2 = zenoh.ros2(Ros2Node::new("cell4_gateway")).register::<Temperature>();
    /// builder.with_connector(zenoh).with_connector(ros2);
    /// ```
    #[cfg(feature = "std")]
    pub fn ros2(&self, node: crate::Ros2Node) -> crate::Ros2Connector {
        crate::Ros2Connector::on(self.backend.shared.clone(), node)
    }
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::Native {}
}

/// A backend with a build path compiled in.
///
/// Implemented for [`Native`] only under `std`.
#[diagnostic::on_unimplemented(
    message = "`ZenohConnector<{Self}>` has no Zenoh backend compiled in",
    label = "no backend for this configuration",
    note = "enable this crate's `std` feature for the `zenoh` crate backend"
)]
pub trait Backend: sealed::Sealed + Send + Sync {
    /// Build this backend's session task.
    fn build<'a>(&'a self, db: &'a AimDb, endpoint: &'a str) -> BuildFuture<'a>;
}

#[cfg(feature = "std")]
impl Backend for Native {
    fn build<'a>(&'a self, db: &'a AimDb, _endpoint: &'a str) -> BuildFuture<'a> {
        crate::native::build(db, &self.shared)
    }
}

impl<B: Backend> ConnectorBuilder for ZenohConnector<B> {
    fn build<'a>(&'a self, db: &'a AimDb) -> BuildFuture<'a> {
        self.backend.build(db, &self.endpoint)
    }

    fn scheme(&self) -> &str {
        crate::SCHEME
    }

    fn owns_scheme(&self) -> bool {
        true
    }
}
