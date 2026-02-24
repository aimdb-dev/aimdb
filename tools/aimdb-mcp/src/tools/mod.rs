//! MCP tools implementation
//!
//! Tools for discovering and interacting with AimDB instances.

use crate::connection::ConnectionPool;
use once_cell::sync::OnceCell;

pub mod architecture;
pub mod graph;
pub mod instance;
pub mod record;
pub mod schema;

// Global connection pool (initialized once)
static CONNECTION_POOL: OnceCell<ConnectionPool> = OnceCell::new();

/// Initialize the connection pool for tools
pub fn init_connection_pool(pool: ConnectionPool) {
    CONNECTION_POOL.set(pool).ok();
}

/// Get the connection pool
pub(crate) fn connection_pool() -> Option<&'static ConnectionPool> {
    CONNECTION_POOL.get()
}

// Re-export tool functions
pub use architecture::{
    get_architecture, get_buffer_metrics, propose_add_connector, propose_add_record,
    propose_modify_buffer, propose_modify_fields, propose_modify_key_variants, remove_record,
    rename_record, reset_session, resolve_proposal, save_memory, validate_against_instance,
};
pub use graph::{graph_edges, graph_nodes, graph_topo_order};
pub use instance::{discover_instances, get_instance_info};
pub use record::{drain_record, get_record, list_records, set_record};
pub use schema::query_schema;
