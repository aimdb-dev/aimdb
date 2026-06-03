//! Remote Access Demo - Client
//!
//! Connects to the demo server over the engine-based [`AimxConnection`] (the
//! shared session engine + reshaped AimX-v2 wire) and walks through the AimX
//! surface: list / get / set, the producer-override safety check, `record.drain`
//! history, and a live subscription.
//!
//! Run with:
//! ```
//! cargo run --bin client
//! ```

use std::time::Duration;

use aimdb_client::AimxConnection;
use futures::StreamExt;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let socket_path = "/tmp/aimdb-demo.sock";
    println!("🔌 Connecting to AimDB server at {socket_path} ...");

    let conn = AimxConnection::connect(socket_path).await.map_err(|e| {
        format!("Failed to connect to {socket_path}: {e}\nMake sure the server is running!")
    })?;

    let welcome = conn.server_info();
    println!("✅ Connected! Welcome from server: {}", welcome.server);
    println!("   Version: {}", welcome.version);
    println!("   Permissions: {:?}", welcome.permissions);
    println!("   Writable records: {:?}", welcome.writable_records);
    println!();

    // ── record.list ──────────────────────────────────────────────────────
    println!("📤 Requesting record list...");
    let records = conn.list_records().await?;
    println!("📋 {} registered records:", records.len());
    for r in &records {
        println!("   • {} ({})", r.record_key, r.name);
    }
    println!();

    // ── Point-in-time reads: record.get ──────────────────────────────────
    // `record.get` is a point-in-time read. For a SingleLatest/state record it
    // returns the canonical latest, non-destructively.
    println!("📤 record.get on Config (SingleLatest — point-in-time read)...");
    match conn.get_record("server::Config").await {
        Ok(v) => println!("⚙️  Current Config:\n{}", serde_json::to_string_pretty(&v)?),
        Err(e) => println!("❌ Error: {e}"),
    }
    println!();

    // A ring (SpmcRing) has no canonical latest, so the server falls back to
    // *draining* this connection's cursor and returning the most recent value. A
    // fresh cursor starts at the ring tail, so the first read is empty until a new
    // value arrives — we retry over a couple of ticks. (This opens the *same*
    // cursor `record.drain` uses below.)
    println!("📤 record.get on Temperature (SpmcRing — drains the cursor for the latest)...");
    let mut latest = None;
    for _ in 0..10 {
        if let Ok(v) = conn.get_record("server::Temperature").await {
            latest = Some(v);
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    match latest {
        Some(v) => println!(
            "🌡️  Most recent Temperature (via drain fallback):\n{}",
            serde_json::to_string_pretty(&v)?
        ),
        None => println!("ℹ️  No reading yet — the ring cursor was still empty."),
    }
    println!();

    // ── record.set (write operations) ────────────────────────────────────
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("✍️  Testing record.set (Write Operations)");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    println!("📤 Current AppSettings:");
    match conn.get_record("server::AppSettings").await {
        Ok(v) => println!("{}\n", serde_json::to_string_pretty(&v)?),
        Err(e) => {
            println!("❌ Error: {e}");
            return Ok(());
        }
    }

    println!("📤 Updating AppSettings (enabling feature_flag_alpha)...");
    let new_settings = json!({
        "log_level": "debug",
        "max_connections": 200,
        "feature_flag_alpha": true
    });
    match conn.set_record("server::AppSettings", new_settings).await {
        Ok(v) => println!("✅ record.set ok:\n{}\n", serde_json::to_string_pretty(&v)?),
        Err(e) => {
            println!("❌ Error: {e}");
            return Ok(());
        }
    }

    println!("📤 Verifying update...");
    match conn.get_record("server::AppSettings").await {
        Ok(v) => println!(
            "✔️  AppSettings after update:\n{}\n",
            serde_json::to_string_pretty(&v)?
        ),
        Err(e) => println!("❌ Error: {e}"),
    }

    // ── Safety: overriding a record with a producer must be denied ────────
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("🛡️  Safety: try to override Temperature (has producer)");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    println!("📤 Attempting to set Temperature (SHOULD FAIL)...");
    match conn
        .set_record(
            "server::Temperature",
            json!({ "sensor_id": "hacked", "celsius": 999.9, "timestamp": 0.0 }),
        )
        .await
    {
        Ok(v) => println!("❌ UNEXPECTED success — producer protection failed: {v}"),
        Err(_) => println!("✅ Expected failure — cannot override a record with a producer.\n"),
    }

    // ── record.drain (history) ───────────────────────────────────────────
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("🧪 Record History (record.drain)");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    // `record.get` above already opened this connection's Temperature cursor, so
    // this drain returns only what's accrued since (they share one cursor).
    println!("📤 Drain #1: history since the (already-open) cursor...");
    let d1 = conn.drain_record("server::Temperature").await?;
    println!("   Values: {} (cursor shared with the record.get above)\n", d1.count);

    println!("⏳ Waiting 7s for temperature readings to accumulate...");
    tokio::time::sleep(Duration::from_secs(7)).await;

    println!("📤 Drain #2: accumulated history...");
    let d2 = conn.drain_record("server::Temperature").await?;
    println!("   Values: {} (expected ~3)", d2.count);
    for (i, v) in d2.values.iter().enumerate() {
        let celsius = v["celsius"].as_f64().unwrap_or(0.0);
        let sensor = v["sensor_id"].as_str().unwrap_or("?");
        println!("   📊 [{i}] {celsius:.1} °C from {sensor}");
    }
    println!();

    println!("📤 Drain #3: immediate re-drain (should be empty)...");
    let d3 = conn.drain_record("server::Temperature").await?;
    println!("   Values: {} (expected 0)\n", d3.count);

    println!("⏳ Waiting 5s, then draining with limit=2...");
    tokio::time::sleep(Duration::from_secs(5)).await;
    let d4 = conn
        .drain_record_with_limit("server::Temperature", 2)
        .await?;
    println!("   Values: {} (limit was 2)\n", d4.count);

    // ── Subscriptions ────────────────────────────────────────────────────
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("📡 Subscriptions");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    println!("📤 Subscribing to Temperature (will receive 5 events)...");
    let mut stream = conn.subscribe("server::Temperature")?;
    for i in 1..=5 {
        match stream.next().await {
            Some(v) => println!("📨 Event #{i}: {}", serde_json::to_string(&v)?),
            None => {
                println!("⚠️  Stream ended early");
                break;
            }
        }
    }
    // Dropping the stream stops local delivery (no explicit unsubscribe needed).
    drop(stream);

    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("👋 Disconnecting...");
    Ok(())
}
