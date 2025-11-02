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
struct EventMessage {
    event: Event,
}

#[derive(Debug, Deserialize)]
struct Event {
    subscription_id: String,
    sequence: u64,
    timestamp: String,
    data: serde_json::Value,
    #[serde(default)]
    dropped: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ErrorObject {
    code: String,
    message: String,
    #[serde(default)]
    details: Option<serde_json::Value>,
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
    #[allow(dead_code)] // Parsed from JSON but not used in demo
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

    // Test record.get for Temperature
    println!("📤 Requesting Temperature value...");
    let get_request = Request {
        id: 2,
        method: "record.get".to_string(),
        params: Some(json!({"record": "server::Temperature"})),
    };

    let get_request_json = serde_json::to_string(&get_request)?;
    writeln!(stream, "{}", get_request_json)?;
    stream.flush()?;

    // Read response
    let mut get_response_line = String::new();
    reader.read_line(&mut get_response_line)?;

    let get_response: Response = serde_json::from_str(&get_response_line)?;

    match get_response {
        Response::Success { id, result } => {
            println!("✅ Success! (request_id: {})", id);
            println!();
            println!("🌡️  Current Temperature:");
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

    // Test record.get for SystemStatus
    println!("📤 Requesting SystemStatus value...");
    let status_request = Request {
        id: 3,
        method: "record.get".to_string(),
        params: Some(json!({"record": "server::SystemStatus"})),
    };

    let status_request_json = serde_json::to_string(&status_request)?;
    writeln!(stream, "{}", status_request_json)?;
    stream.flush()?;

    let mut status_response_line = String::new();
    reader.read_line(&mut status_response_line)?;
    let status_response: Response = serde_json::from_str(&status_response_line)?;

    match status_response {
        Response::Success { id, result } => {
            println!("✅ Success! (request_id: {})", id);
            println!();
            println!("💻 Current System Status:");
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Response::Error { id, error } => {
            println!("❌ Error! (request_id: {})", id);
            println!("   Code: {}", error.code);
            println!("   Message: {}", error.message);
        }
    }

    println!();

    // Test record.get for Config
    println!("📤 Requesting Config value...");
    let config_request = Request {
        id: 4,
        method: "record.get".to_string(),
        params: Some(json!({"record": "server::Config"})),
    };

    let config_request_json = serde_json::to_string(&config_request)?;
    writeln!(stream, "{}", config_request_json)?;
    stream.flush()?;

    let mut config_response_line = String::new();
    reader.read_line(&mut config_response_line)?;
    let config_response: Response = serde_json::from_str(&config_response_line)?;

    match config_response {
        Response::Success { id, result } => {
            println!("✅ Success! (request_id: {})", id);
            println!();
            println!("⚙️  Current Config:");
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Response::Error { id, error } => {
            println!("❌ Error! (request_id: {})", id);
            println!("   Code: {}", error.code);
            println!("   Message: {}", error.message);
        }
    }

    println!();

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("📡 Testing Subscriptions");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    // Subscribe to Temperature updates
    println!("📤 Subscribing to Temperature updates...");
    let subscribe_request = Request {
        id: 5,
        method: "record.subscribe".to_string(),
        params: Some(json!({
            "name": "server::Temperature",
            "queue_size": 50
        })),
    };

    let subscribe_json = serde_json::to_string(&subscribe_request)?;
    writeln!(stream, "{}", subscribe_json)?;
    stream.flush()?;

    // Read subscription response
    let mut subscribe_response_line = String::new();
    reader.read_line(&mut subscribe_response_line)?;

    let subscribe_response: Response = serde_json::from_str(&subscribe_response_line)?;

    let subscription_id = match subscribe_response {
        Response::Success { id, result } => {
            println!("✅ Subscribed! (request_id: {})", id);
            let sub_id = result["subscription_id"].as_str().unwrap().to_string();
            let queue_size = result["queue_size"].as_u64().unwrap();
            println!("   Subscription ID: {}", sub_id);
            println!("   Queue Size: {}", queue_size);
            println!();
            println!("📊 Receiving live temperature updates (will receive 5 events)...");
            println!();
            sub_id
        }
        Response::Error { id, error } => {
            println!("❌ Subscription failed! (request_id: {})", id);
            println!("   Code: {}", error.code);
            println!("   Message: {}", error.message);
            return Ok(());
        }
    };

    // Receive 5 events
    for i in 1..=5 {
        let mut event_line = String::new();
        reader.read_line(&mut event_line)?;

        // Try to parse as EventMessage
        if let Ok(event_msg) = serde_json::from_str::<EventMessage>(&event_line) {
            let event = event_msg.event;
            println!("📨 Event #{} (seq: {})", i, event.sequence);
            println!("   Subscription: {}", event.subscription_id);
            println!("   Timestamp: {}", event.timestamp);
            if let Some(dropped) = event.dropped {
                println!("   ⚠️  Dropped events: {}", dropped);
            }
            println!("   Data: {}", serde_json::to_string_pretty(&event.data)?);
            println!();
        } else {
            println!("⚠️  Received unexpected message: {}", event_line.trim());
        }

        // Small delay to show streaming behavior
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    // Unsubscribe
    println!("📤 Unsubscribing from Temperature...");
    let unsubscribe_request = Request {
        id: 6,
        method: "record.unsubscribe".to_string(),
        params: Some(json!({
            "subscription_id": subscription_id
        })),
    };

    let unsubscribe_json = serde_json::to_string(&unsubscribe_request)?;
    writeln!(stream, "{}", unsubscribe_json)?;
    stream.flush()?;

    // Read unsubscribe response
    let mut unsubscribe_response_line = String::new();
    reader.read_line(&mut unsubscribe_response_line)?;

    // Parse response - filter out any stray events
    let unsubscribe_response: Result<Response, _> =
        serde_json::from_str(&unsubscribe_response_line);

    match unsubscribe_response {
        Ok(Response::Success { id, result }) => {
            println!("✅ Unsubscribed! (request_id: {})", id);
            println!(
                "   Status: {}",
                result["status"].as_str().unwrap_or("unknown")
            );
            println!();
        }
        Ok(Response::Error { id, error }) => {
            println!("❌ Unsubscribe failed! (request_id: {})", id);
            println!("   Code: {}", error.code);
            println!("   Message: {}", error.message);
        }
        Err(_) => {
            // Might be a stray event, try reading next line
            println!("⚠️  Received unexpected message, retrying...");
            let mut retry_line = String::new();
            reader.read_line(&mut retry_line)?;
            match serde_json::from_str::<Response>(&retry_line) {
                Ok(Response::Success { id, result }) => {
                    println!("✅ Unsubscribed! (request_id: {})", id);
                    println!(
                        "   Status: {}",
                        result["status"].as_str().unwrap_or("unknown")
                    );
                    println!();
                }
                Ok(Response::Error { id, error }) => {
                    println!("❌ Unsubscribe failed! (request_id: {})", id);
                    println!("   Code: {}", error.code);
                    println!("   Message: {}", error.message);
                }
                Err(e) => {
                    println!("⚠️  Failed to parse unsubscribe response: {}", e);
                }
            }
        }
    }

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();
    println!("👋 Disconnecting...");

    Ok(())
}
