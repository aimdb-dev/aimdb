//! AimDB MCP Server
//!
//! Model Context Protocol (MCP) server implementation for AimDB.
//! Enables Large Language Models to interact with running AimDB instances
//! for introspection, debugging, and monitoring.
//!
//! # Architecture
//!
//! ```text
//! LLM Host (VS Code/Claude)
//!   ↓ stdio (JSON-RPC 2.0)
//! aimdb-mcp server
//!   ↓ aimdb-client library
//! AimDB instances (Unix sockets)
//! ```
//!
//! # MCP Protocol
//!
//! - **Transport**: stdio with NDJSON
//! - **Protocol**: JSON-RPC 2.0
//! - **Version**: 2025-06-18
//! - **Capabilities**: Tools (8), Resources (5), Prompts (3)

pub mod connection;
pub mod error;
pub mod notification_writer;
pub mod prompts;
pub mod protocol;
pub mod resources;
pub mod server;
pub mod subscription_manager;
pub mod tools;
pub mod transport;

pub use error::{McpError, McpResult};
pub use notification_writer::NotificationFileWriter;
pub use server::McpServer;
pub use transport::StdioTransport;
