//! MCP tools implementation
//!
//! Tools for discovering and interacting with AimDB instances.

use crate::connection::ConnectionPool;
use once_cell::sync::OnceCell;

pub mod buffer_metrics;
pub mod graph;
pub mod instance;
pub mod profiling;
pub mod record;

// Global connection pool (initialized once)
static CONNECTION_POOL: OnceCell<ConnectionPool> = OnceCell::new();

// Default endpoint set by --connect at startup (takes precedence over the
// AIMDB_CONNECT env var)
static DEFAULT_ENDPOINT: OnceCell<String> = OnceCell::new();

/// Initialize the connection pool for tools
pub fn init_connection_pool(pool: ConnectionPool) {
    CONNECTION_POOL.set(pool).ok();
}

/// Set the default endpoint (called once at startup from the --connect flag).
pub fn set_default_endpoint(endpoint: String) {
    DEFAULT_ENDPOINT.set(endpoint).ok();
}

/// Get the connection pool
pub(crate) fn connection_pool() -> Option<&'static ConnectionPool> {
    CONNECTION_POOL.get()
}

/// The persistent client for `endpoint`, the only way tools reach an instance.
/// Callers hold its lock for the duration of one call, so requests on a session
/// never overlap.
pub(crate) async fn shared_client(
    endpoint: &str,
) -> crate::error::McpResult<crate::connection::SharedClient> {
    let pool = connection_pool().ok_or_else(|| {
        crate::error::McpError::Internal("Connection pool not initialized".to_string())
    })?;
    pool.get_client(endpoint)
        .await
        .map_err(crate::error::McpError::Client)
}

/// Resolve the endpoint from an explicit argument, the `--connect` flag, or the
/// `AIMDB_CONNECT` env var (checked in that order). The value is a `scheme://`
/// URL (`unix://PATH`, `serial://DEVICE?baud=N`) or a bare path.
///
/// Returns an error if none are set.
pub(crate) fn resolve_endpoint(explicit: Option<String>) -> crate::error::McpResult<String> {
    explicit
        .or_else(|| DEFAULT_ENDPOINT.get().cloned())
        .or_else(|| std::env::var("AIMDB_CONNECT").ok())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            crate::error::McpError::InvalidParams(
                "Missing endpoint (pass it explicitly, use --connect, or set AIMDB_CONNECT env var)"
                    .into(),
            )
        })
}

// Re-export tool functions
pub use buffer_metrics::{get_buffer_metrics, reset_buffer_metrics};
pub use graph::{graph_edges, graph_nodes, graph_topo_order};
pub use instance::{discover_instances, get_instance_info};
pub use profiling::{get_stage_profiling, reset_stage_profiling};
pub use record::{drain_record, get_record, list_records, set_record};
