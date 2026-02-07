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
    println!("✍️  Testing record.set (Write Operations)");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    // Test 1: Get current AppSettings
    println!("📤 Getting current AppSettings...");
    let get_settings_request = Request {
        id: 5,
        method: "record.get".to_string(),
        params: Some(json!({"record": "server::AppSettings"})),
    };

    writeln!(stream, "{}", serde_json::to_string(&get_settings_request)?)?;
    stream.flush()?;

    let mut settings_response_line = String::new();
    reader.read_line(&mut settings_response_line)?;
    let settings_response: Response = serde_json::from_str(&settings_response_line)?;

    match settings_response {
        Response::Success { id, result } => {
            println!("✅ Success! (request_id: {})", id);
            println!();
            println!("⚙️  Original AppSettings:");
            println!("{}", serde_json::to_string_pretty(&result)?);
            println!();
        }
        Response::Error { id, error } => {
            println!("❌ Error! (request_id: {})", id);
            println!("   Code: {}", error.code);
            println!("   Message: {}", error.message);
            return Ok(());
        }
    };

    // Test 2: Modify and set new AppSettings
    println!("📤 Updating AppSettings (enabling feature_flag_alpha)...");
    let new_settings = json!({
        "log_level": "debug",
        "max_connections": 200,
        "feature_flag_alpha": true
    });

    let set_request = Request {
        id: 6,
        method: "record.set".to_string(),
        params: Some(json!({
            "name": "server::AppSettings",
            "value": new_settings
        })),
    };

    writeln!(stream, "{}", serde_json::to_string(&set_request)?)?;
    stream.flush()?;

    let mut set_response_line = String::new();
    reader.read_line(&mut set_response_line)?;
    let set_response: Response = serde_json::from_str(&set_response_line)?;

    match set_response {
        Response::Success { id, result } => {
            println!("✅ Success! record.set completed (request_id: {})", id);
            println!();
            println!("✨ Updated AppSettings:");
            println!("{}", serde_json::to_string_pretty(&result)?);
            println!();
        }
        Response::Error { id, error } => {
            println!("❌ Error! (request_id: {})", id);
            println!("   Code: {}", error.code);
            println!("   Message: {}", error.message);
            if let Some(details) = error.details {
                println!("   Details: {}", details);
            }
            return Ok(());
        }
    }

    // Test 3: Verify the change by getting again
    println!("📤 Verifying update by getting AppSettings again...");
    let verify_request = Request {
        id: 7,
        method: "record.get".to_string(),
        params: Some(json!({"record": "server::AppSettings"})),
    };

    writeln!(stream, "{}", serde_json::to_string(&verify_request)?)?;
    stream.flush()?;

    let mut verify_response_line = String::new();
    reader.read_line(&mut verify_response_line)?;
    let verify_response: Response = serde_json::from_str(&verify_response_line)?;

    match verify_response {
        Response::Success { id, result } => {
            println!("✅ Success! (request_id: {})", id);
            println!();
            println!("✔️  Verified - AppSettings after update:");
            println!("{}", serde_json::to_string_pretty(&result)?);
            println!();
        }
        Response::Error { id, error } => {
            println!("❌ Error! (request_id: {})", id);
            println!("   Code: {}", error.code);
            println!("   Message: {}", error.message);
        }
    }

    // Test 4: Try to set Temperature (should fail - has producer)
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("🛡️  Testing Safety: Try to override Temperature (has producer)");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    println!("📤 Attempting to set Temperature (SHOULD FAIL)...");
    let bad_set_request = Request {
        id: 8,
        method: "record.set".to_string(),
        params: Some(json!({
            "name": "server::Temperature",
            "value": {
                "sensor_id": "hacked-sensor",
                "celsius": 999.9,
                "timestamp": 0
            }
        })),
    };

    writeln!(stream, "{}", serde_json::to_string(&bad_set_request)?)?;
    stream.flush()?;

    let mut bad_set_response_line = String::new();
    reader.read_line(&mut bad_set_response_line)?;
    let bad_set_response: Response = serde_json::from_str(&bad_set_response_line)?;

    match bad_set_response {
        Response::Success { id, result } => {
            println!("❌ UNEXPECTED! record.set succeeded when it should have failed!");
            println!("   Request ID: {}", id);
            println!("   Result: {}", result);
            println!("   ⚠️  This is a security issue - producer protection not working!");
        }
        Response::Error { id, error } => {
            println!(
                "✅ EXPECTED FAILURE! Safety check worked (request_id: {})",
                id
            );
            println!("   Code: {}", error.code);
            println!("   Message: {}", error.message);
            println!("   🛡️  Protection confirmed: Cannot override records with producers");
            if let Some(details) = error.details {
                println!("   Details: {}", details);
            }
        }
    }

    println!();

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("� Testing Record History (record.drain)");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    // Drain #1: Cold start — creates the drain reader, returns empty
    println!("📤 Drain #1: Creating drain reader for Temperature (cold start)...");
    let drain1_request = Request {
        id: 9,
        method: "record.drain".to_string(),
        params: Some(json!({"name": "server::Temperature"})),
    };

    writeln!(stream, "{}", serde_json::to_string(&drain1_request)?)?;
    stream.flush()?;

    let mut drain1_line = String::new();
    reader.read_line(&mut drain1_line)?;
    let drain1_response: Response = serde_json::from_str(&drain1_line)?;

    match &drain1_response {
        Response::Success { id, result } => {
            let count = result["count"].as_u64().unwrap_or(0);
            println!("✅ Drain #1 response (request_id: {})", id);
            println!("   Values returned: {} (expected 0 on cold start)", count);
            println!();
        }
        Response::Error { id, error } => {
            println!("❌ Drain #1 failed! (request_id: {})", id);
            println!("   Code: {}", error.code);
            println!("   Message: {}", error.message);
        }
    }

    // Wait for values to accumulate (server produces every 2s)
    println!("⏳ Waiting 7 seconds for temperature readings to accumulate...");
    std::thread::sleep(std::time::Duration::from_secs(7));

    // Drain #2: Should return accumulated values (~3 readings at 2s interval)
    println!("📤 Drain #2: Fetching accumulated Temperature history...");
    let drain2_request = Request {
        id: 10,
        method: "record.drain".to_string(),
        params: Some(json!({"name": "server::Temperature"})),
    };

    writeln!(stream, "{}", serde_json::to_string(&drain2_request)?)?;
    stream.flush()?;

    let mut drain2_line = String::new();
    reader.read_line(&mut drain2_line)?;
    let drain2_response: Response = serde_json::from_str(&drain2_line)?;

    match &drain2_response {
        Response::Success { id, result } => {
            let count = result["count"].as_u64().unwrap_or(0);
            let values = result["values"].as_array();
            println!("✅ Drain #2 response (request_id: {})", id);
            println!("   Values returned: {} (expected ~3)", count);
            if let Some(vals) = values {
                for (i, val) in vals.iter().enumerate() {
                    let celsius = val["celsius"].as_f64().unwrap_or(0.0);
                    let sensor = val["sensor_id"].as_str().unwrap_or("?");
                    println!("   📊 [{}] {:.1} °C from {}", i, celsius, sensor);
                }
            }
            println!();
        }
        Response::Error { id, error } => {
            println!("❌ Drain #2 failed! (request_id: {})", id);
            println!("   Code: {}", error.code);
            println!("   Message: {}", error.message);
        }
    }

    // Drain #3: Immediately after — should be empty (nothing new since last drain)
    println!("📤 Drain #3: Immediate re-drain (should be empty)...");
    let drain3_request = Request {
        id: 11,
        method: "record.drain".to_string(),
        params: Some(json!({"name": "server::Temperature"})),
    };

    writeln!(stream, "{}", serde_json::to_string(&drain3_request)?)?;
    stream.flush()?;

    let mut drain3_line = String::new();
    reader.read_line(&mut drain3_line)?;
    let drain3_response: Response = serde_json::from_str(&drain3_line)?;

    match &drain3_response {
        Response::Success { id, result } => {
            let count = result["count"].as_u64().unwrap_or(0);
            println!("✅ Drain #3 response (request_id: {})", id);
            println!(
                "   Values returned: {} (expected 0 — nothing new since last drain)",
                count
            );
            println!();
        }
        Response::Error { id, error } => {
            println!("❌ Drain #3 failed! (request_id: {})", id);
            println!("   Code: {}", error.code);
            println!("   Message: {}", error.message);
        }
    }

    // Drain #4: Test with limit parameter
    println!("⏳ Waiting 5 seconds, then draining with limit=2...");
    std::thread::sleep(std::time::Duration::from_secs(5));

    let drain4_request = Request {
        id: 12,
        method: "record.drain".to_string(),
        params: Some(json!({
            "name": "server::Temperature",
            "limit": 2
        })),
    };

    writeln!(stream, "{}", serde_json::to_string(&drain4_request)?)?;
    stream.flush()?;

    let mut drain4_line = String::new();
    reader.read_line(&mut drain4_line)?;
    let drain4_response: Response = serde_json::from_str(&drain4_line)?;

    match &drain4_response {
        Response::Success { id, result } => {
            let count = result["count"].as_u64().unwrap_or(0);
            let values = result["values"].as_array();
            println!("✅ Drain #4 response (request_id: {})", id);
            println!("   Values returned: {} (limit was 2)", count);
            if let Some(vals) = values {
                for (i, val) in vals.iter().enumerate() {
                    let celsius = val["celsius"].as_f64().unwrap_or(0.0);
                    let sensor = val["sensor_id"].as_str().unwrap_or("?");
                    println!("   📊 [{}] {:.1} °C from {}", i, celsius, sensor);
                }
            }
            println!();
        }
        Response::Error { id, error } => {
            println!("❌ Drain #4 failed! (request_id: {})", id);
            println!("   Code: {}", error.code);
            println!("   Message: {}", error.message);
        }
    }

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("�📡 Testing Subscriptions");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    // Subscribe to Temperature updates
    println!("📤 Subscribing to Temperature updates...");
    let subscribe_request = Request {
        id: 13,
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
        id: 14,
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
