//! MCP tools implementation
//!
//! Tools for discovering and interacting with AimDB instances.

use crate::connection::ConnectionPool;
use once_cell::sync::OnceCell;

pub mod architecture;
pub mod buffer_metrics;
pub mod graph;
pub mod instance;
pub mod profiling;
pub mod record;
pub mod schema;

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
pub use architecture::{
    get_architecture, propose_add_binary, propose_add_connector, propose_add_record,
    propose_add_task, propose_modify_buffer, propose_modify_fields, propose_modify_key_variants,
    remove_binary, remove_record, remove_task, rename_record, reset_session, resolve_proposal,
    save_memory, validate_against_instance,
};
pub use buffer_metrics::{get_buffer_metrics, reset_buffer_metrics};
pub use graph::{graph_edges, graph_nodes, graph_topo_order};
pub use instance::{discover_instances, get_instance_info};
pub use profiling::{get_stage_profiling, reset_stage_profiling};
pub use record::{drain_record, get_record, list_records, set_record};
pub use schema::query_schema;
