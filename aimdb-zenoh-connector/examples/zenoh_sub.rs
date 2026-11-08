//! Prints every sample on a Zenoh key expression as text, through the router
//! at `ZENOH_ENDPOINT` (default `tcp/127.0.0.1:7447`); a stand-in for
//! `z_sub` when the Zenoh tools are not installed.
//!
//! ```text
//! cargo run -p aimdb-zenoh-connector --features std --example zenoh_sub -- 'telemetry/**'
//! ```

#[tokio::main]
async fn main() {
    let endpoint = std::env::var("ZENOH_ENDPOINT").unwrap_or_else(|_| "tcp/127.0.0.1:7447".into());
    let key = std::env::args().nth(1).unwrap_or_else(|| "**".into());

    let mut config = zenoh::Config::default();
    config.insert_json5("mode", r#""client""#).unwrap();
    config
        .insert_json5("connect/endpoints", &format!(r#"["{endpoint}"]"#))
        .unwrap();
    config
        .insert_json5("scouting/multicast/enabled", "false")
        .unwrap();

    let session = zenoh::open(config).await.expect("open");
    let subscriber = session.declare_subscriber(&key).await.expect("subscribe");
    println!("zenoh_sub: {key} on {endpoint}");
    while let Ok(sample) = subscriber.recv_async().await {
        let payload = sample.payload().to_bytes();
        println!(
            "{}: {}",
            sample.key_expr(),
            String::from_utf8_lossy(&payload)
        );
    }
}
