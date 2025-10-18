//! Visionary synchronous main for a weather-station app using AimDB as
//! the central async datastore. AimDB runs on its own thread while this
//! main remains fully synchronous and interacts via thread-safe handles.

mod record_definition;

use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use record_definition::{weather_alert_record, weather_reading_record, WeatherReading};

/// Entry point for an IoT edge app collecting and processing weather data.
///
/// Goals:
/// - Keep main() synchronous
/// - Run AimDB (async) in a dedicated thread
/// - Interact with data using simple, thread-safe db.set/db.get APIs
fn main() {
	// 1) Spawn AimDB on a separate thread and obtain a thread-safe handle
	//    The handle exposes simple get/set and blocking iteration APIs.
	let db = aimdb_core::AimDb::builder()
		.register(weather_reading_record())
		.register(weather_alert_record())
        .build()
		.run()
		.expect("spawn AimDB on dedicated thread");

	// 2) Run a sensor mock loop in the main thread that writes directly via db.set.
	println!("AimDB running on a dedicated thread. Sensor mock active in main thread.");
	let mut temp = 19.5_f32;
	let mut pressure = 1013.0_f32;
	let humidity = 48.0_f32;
	loop {
		temp += 0.1; // drift
		pressure += if (temp as i32) % 20 == 0 { -0.5 } else { 0.0 };

		let now_ms = SystemTime::now()
			.duration_since(UNIX_EPOCH)
			.unwrap_or_default()
			.as_millis() as u64;

		let reading = WeatherReading::new(42, temp, humidity, pressure, now_ms);

		// Thread-safe, blocking write into AimDB via set::<T>()
		if let Err(e) = db.set::<WeatherReading>(reading) {
			eprintln!("sensor_mock: write error: {e:?}");
		}

		thread::sleep(Duration::from_millis(500));
	}
}

