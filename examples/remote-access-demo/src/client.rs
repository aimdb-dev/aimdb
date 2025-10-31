//! Remote Access Demo - Client
//!
//! Simple client that connects to the demo server and calls record.list
//!
//! Run with:
//! ```
//! cargo run --bin client
//! ```

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

#[derive(Debug, Serialize)]
struct Request {
    id: u64,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Response {
    Success { id: u64, result: serde_json::Value },
    Error { id: u64, error: ErrorObject },
}

#[derive(Debug, Deserialize)]
struct ErrorObject {
    code: String,
    message: String,
    #[serde(default)]
    details: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct HelloMessage {
    version: String,
    client: String,
    #[serde(default)]
    capabilities: Option<Vec<String>>,
    #[serde(default)]
    auth_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WelcomeMessage {
    version: String,
    server: String,
    permissions: Vec<String>,
    writable_records: Vec<String>,
    #[serde(default)]
    max_subscriptions: Option<usize>,
    #[serde(default)]
    authenticated: Option<bool>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🔌 Connecting to AimDB server...");

    let socket_path = "/tmp/aimdb-demo.sock";
    let mut stream = UnixStream::connect(socket_path).map_err(|e| {
        format!(
            "Failed to connect to {}: {}\nMake sure the server is running!",
            socket_path, e
        )
    })?;

    let mut reader = BufReader::new(stream.try_clone()?);

    println!("✅ Connected!");
    println!();

    // Send Hello message
    println!("📤 Sending handshake...");
    let hello = json!({
        "version": "1.0",
        "client": "aimdb-demo-client",
        "capabilities": [],
    });

    writeln!(stream, "{}", hello)?;
    stream.flush()?;

    // Read Welcome message
    let mut line = String::new();
    reader.read_line(&mut line)?;

    let welcome: WelcomeMessage = serde_json::from_str(&line)?;
    println!("📥 Received welcome from server: {}", welcome.server);
    println!("   Version: {}", welcome.version);
    println!("   Permissions: {:?}", welcome.permissions);
    println!("   Writable records: {:?}", welcome.writable_records);
    println!("   Max subscriptions: {:?}", welcome.max_subscriptions);
    println!();

    // Send record.list request
    println!("📤 Requesting record list...");
    let request = Request {
        id: 1,
        method: "record.list".to_string(),
        params: None,
    };

    let request_json = serde_json::to_string(&request)?;
    writeln!(stream, "{}", request_json)?;
    stream.flush()?;

    // Read response
    let mut response_line = String::new();
    reader.read_line(&mut response_line)?;

    let response: Response = serde_json::from_str(&response_line)?;

    match response {
        Response::Success { id, result } => {
            println!("✅ Success! (request_id: {})", id);
            println!();
            println!("📋 Registered Records:");
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Response::Error { id, error } => {
            println!("❌ Error! (request_id: {})", id);
            println!("   Code: {}", error.code);
            println!("   Message: {}", error.message);
            if let Some(details) = error.details {
                println!("   Details: {}", details);
            }
        }
    }

    println!();
    println!("👋 Disconnecting...");

    Ok(())
}
