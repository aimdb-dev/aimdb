//! MCP tools implementation
//!
//! Tools for discovering and interacting with AimDB instances.

use crate::connection::ConnectionPool;
use crate::subscription_manager::SubscriptionManager;
use once_cell::sync::OnceCell;
use std::path::PathBuf;
use std::sync::Arc;

pub mod instance;
pub mod record;
pub mod schema;
pub mod subscription;

// Global connection pool (initialized once)
static CONNECTION_POOL: OnceCell<ConnectionPool> = OnceCell::new();

// Global subscription manager (initialized once)
static SUBSCRIPTION_MANAGER: OnceCell<Arc<SubscriptionManager>> = OnceCell::new();

// Global notification directory (initialized once)
static NOTIFICATION_DIR: OnceCell<PathBuf> = OnceCell::new();

/// Initialize the connection pool for tools
pub fn init_connection_pool(pool: ConnectionPool) {
    CONNECTION_POOL.set(pool).ok();
}

/// Get the connection pool
pub(crate) fn connection_pool() -> Option<&'static ConnectionPool> {
    CONNECTION_POOL.get()
}

/// Initialize the subscription manager for tools
pub fn init_subscription_manager(manager: Arc<SubscriptionManager>) {
    SUBSCRIPTION_MANAGER.set(manager).ok();
}

/// Get the subscription manager
pub(crate) fn subscription_manager() -> Option<&'static Arc<SubscriptionManager>> {
    SUBSCRIPTION_MANAGER.get()
}

/// Initialize the notification directory for tools
pub fn init_notification_dir(dir: PathBuf) {
    NOTIFICATION_DIR.set(dir).ok();
}

/// Get the notification directory
pub(crate) fn notification_dir() -> Option<&'static PathBuf> {
    NOTIFICATION_DIR.get()
}

// Re-export tool functions
pub use instance::{discover_instances, get_instance_info};
pub use record::{get_record, list_records, set_record};
pub use schema::query_schema;
pub use subscription::{
    get_notification_directory, list_subscriptions, subscribe_record, unsubscribe_record,
};
