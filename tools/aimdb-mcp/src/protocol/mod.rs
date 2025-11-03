//! JSON-RPC 2.0 and MCP protocol types
//!
//! Manual implementation (no external JSON-RPC crate) as per design doc.

pub mod jsonrpc;
pub mod mcp;

pub use jsonrpc::*;
pub use mcp::*;
