//! stdio transport for JSON-RPC 2.0 over NDJSON
//!
//! Handles reading JSON-RPC requests from stdin and writing responses to stdout.

use crate::error::McpResult;
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Stdio transport for MCP protocol
pub struct StdioTransport {
    reader: BufReader<io::Stdin>,
    writer: io::Stdout,
}

impl StdioTransport {
    /// Create a new stdio transport
    pub fn new() -> Self {
        Self {
            reader: BufReader::new(io::stdin()),
            writer: io::stdout(),
        }
    }

    /// Read a line from stdin (blocking)
    pub async fn read_line(&mut self) -> McpResult<Option<String>> {
        loop {
            let mut line = String::new();
            match self.reader.read_line(&mut line).await? {
                0 => return Ok(None), // EOF
                _ => {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        return Ok(Some(trimmed.to_string()));
                    }
                    // Empty line, loop continues to read next line
                }
            }
        }
    }

    /// Write a line to stdout
    pub async fn write_line(&mut self, line: &str) -> McpResult<()> {
        self.writer.write_all(line.as_bytes()).await?;
        self.writer.write_all(b"\n").await?;
        self.writer.flush().await?;
        Ok(())
    }
}

impl Default for StdioTransport {
    fn default() -> Self {
        Self::new()
    }
}
