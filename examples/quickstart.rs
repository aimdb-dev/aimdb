//! AimDB Quick Start Example
//!
//! This is a simple demonstration of AimDB concepts.
//! Run with: cargo run --example quickstart

fn main() {
    println!("🚀 AimDB QuickStart Demo");
    println!("======================");

    println!("📡 Initializing AimDB in-memory database...");
    println!("✅ Database initialized");

    println!("\n🏭 Simulating MCU device data:");
    for i in 1..=3 {
        println!("  📊 MCU Device #{} - Temperature: {}°C", i, 20 + i * 2);
    }

    println!("\n🌐 Edge gateway processing:");
    println!("  🔄 Processing sensor data...");
    println!("  📈 Average temperature: 24°C");

    println!("\n☁️  Cloud synchronization:");
    println!("  📤 Data uploaded to cloud");
    println!("  ✅ All devices synchronized");

    println!("\n✨ Demo completed! AimDB keeps MCU → edge → cloud in sync.");
}
