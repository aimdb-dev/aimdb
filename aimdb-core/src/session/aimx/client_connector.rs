//! AimX **client** connector (Phase 3 server port, std-only) — registers the
//! `aimx://` scheme so records can `.link_to`/`.link_from` an AimX peer, and on
//! build dials that peer and drives the mirroring pumps.
//!
//! This is the registerable wrapper around [`pump_client`](crate::session::pump_client):
//! `build` opens the connection with [`run_client`](crate::session::run_client)
//! and returns one spawn-free future per route (plus the engine future) for the
//! runner to drive — collapsing the AimX *client* onto the shared session engine
//! the same way [`build_aimx_server`](super::build_aimx_server) does the server.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use aimdb_executor::TimeOps;

use crate::builder::AimDb;
use crate::connector::ConnectorBuilder;
use crate::session::{pump_client, run_client, ClientConfig};
use crate::DbResult;

use super::{AimxCodec, UdsDialer};

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// A connector that mirrors records to/from an AimX peer over a Unix-domain
/// socket. Register it with [`AimDbBuilder::with_connector`](crate::AimDbBuilder::with_connector)
/// so `aimx://<record>` links validate; its `build` wires every collected route
/// to the connection.
pub struct AimxClientConnector {
    socket_path: PathBuf,
    config: ClientConfig,
}

impl AimxClientConnector {
    /// Mirror records over the AimX peer listening at `socket_path`.
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
            config: ClientConfig::default(),
        }
    }

    /// Override the client engine config (reconnect policy, etc.).
    pub fn with_config(mut self, config: ClientConfig) -> Self {
        self.config = config;
        self
    }
}

impl<R> ConnectorBuilder<R> for AimxClientConnector
where
    R: TimeOps + 'static,
{
    fn build<'a>(
        &'a self,
        db: &'a AimDb<R>,
    ) -> Pin<Box<dyn Future<Output = DbResult<Vec<BoxFuture>>> + Send + 'a>> {
        Box::pin(async move {
            let (handle, engine_fut) = run_client(
                UdsDialer::new(self.socket_path.clone()),
                AimxCodec,
                self.config.clone(),
                db.runtime_arc(),
            );
            // One pump future per route; they hold `ClientHandle` clones, so the
            // engine stays alive as long as any mirror runs. `handle` drops here.
            let mut futures = pump_client(db, "aimx", &handle);
            futures.push(engine_fut);
            Ok(futures)
        })
    }

    fn scheme(&self) -> &str {
        "aimx"
    }
}
