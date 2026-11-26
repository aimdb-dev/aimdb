//! CLI Command Implementations
//!
//! This module contains the implementations for all CLI commands.

pub mod graph;
pub mod instance;
pub mod record;
pub mod watch;

use std::time::Duration;

use aimdb_client::AimxConnection;

use crate::error::CliResult;

/// The instance a command talks to, and how long each request may take.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Target<'a> {
    /// An explicit endpoint (`--connect`, or the `AIMDB_CONNECT` env), or
    /// `None` for UDS auto-discovery.
    pub endpoint: Option<&'a str>,
    /// Deadline for each request after the handshake (`--request-timeout`).
    pub request_timeout: Duration,
}

impl Target<'_> {
    /// Resolve the target to a live connection.
    ///
    /// An explicit endpoint dials directly — any `scheme://` URL, or a bare
    /// path as the `unix://` shorthand. `None` falls back to UDS
    /// auto-discovery (the first running instance found).
    pub async fn connect(self) -> CliResult<AimxConnection> {
        let conn = match self.endpoint {
            Some(ep) => AimxConnection::connect(ep).await?,
            None => {
                let instance = aimdb_client::discovery::find_instance(None).await?;
                AimxConnection::connect(&instance.endpoint.to_string_lossy()).await?
            }
        };
        Ok(conn.with_request_timeout(self.request_timeout))
    }
}
