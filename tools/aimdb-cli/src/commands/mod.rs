//! CLI Command Implementations
//!
//! This module contains the implementations for all CLI commands.

pub mod generate;
pub mod graph;
pub mod instance;
#[cfg(feature = "join")]
pub mod join;
pub mod record;
pub mod watch;

use aimdb_client::AimxConnection;

use crate::error::CliResult;

/// Resolve an `endpoint` to a live connection.
///
/// An explicit endpoint (`--connect`, or the `AIMDB_CONNECT` env) dials directly —
/// any `scheme://` URL, or a bare path as the `unix://` shorthand. `None` falls
/// back to UDS auto-discovery (the first running instance found).
pub(crate) async fn connect_endpoint(endpoint: Option<&str>) -> CliResult<AimxConnection> {
    match endpoint {
        Some(ep) => Ok(AimxConnection::connect(ep).await?),
        None => {
            let instance = aimdb_client::discovery::find_instance(None).await?;
            Ok(AimxConnection::connect(&instance.endpoint.to_string_lossy()).await?)
        }
    }
}
