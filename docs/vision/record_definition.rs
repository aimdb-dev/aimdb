#[cfg(not(feature = "std"))]
use alloc::string::String;

use serde::{Deserialize, Serialize};
use aimdb_core::{DbResult, RuntimeContext, Consumer, Producer};

// ============================================================================
// Data Structures
// ============================================================================

/// Weather sensor reading - like a database table
/// Stored in AimDB for local processing, not sent over MQTT
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WeatherReading {
    pub sensor_id: u32,
    pub temperature: f32,      // °C
    pub humidity: f32,         // % (0-100)
    pub pressure: f32,         // hPa
    pub wind_speed: f32,       // m/s
    pub wind_direction: u16,   // degrees (0-360)
    pub precipitation: f32,    // mm
    pub timestamp: u64,        // timestamp
}

impl WeatherReading {
    pub fn new(sensor_id: u32, temperature: f32, humidity: f32, pressure: f32, timestamp: u64) -> Self {
        Self {
            sensor_id,
            temperature,
            humidity,
            pressure,
            wind_speed: 0.0,
            wind_direction: 0,
            precipitation: 0.0,
            timestamp,
        }
    }
    
    pub fn heat_index(&self) -> f32 {
        let t = self.temperature;
        let h = self.humidity;
        -8.78469475556 + 1.61139411 * t + 2.33854883889 * h
            + -0.14611605 * t * h + -0.012308094 * t * t + -0.0164248277778 * h * h
    }
    
    pub fn is_storm_conditions(&self) -> bool {
        self.pressure < 980.0 && self.wind_speed > 15.0 && self.precipitation > 5.0
    }
}

/// Weather alert - triggered by dangerous conditions
///
/// The MqttMessage macro provides MQTT transport metadata:
/// - Topic pattern with field interpolation for routing
/// - QoS 2 for guaranteed delivery of critical alerts
/// - Retain flag so new subscribers get last alert state
/// - JSON serialization format
///
/// Publishing is handled explicitly by the `publish_alerts_to_mqtt` consumer task.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WeatherAlert {
    pub alert_id: u32,
    pub sensor_id: u32,
    pub alert_type: AlertType,
    pub severity: AlertSeverity,
    pub message: String,
    pub timestamp: u64,
    pub acknowledged: bool,
}

impl WeatherAlert {
    pub fn new(
        alert_id: u32,
        sensor_id: u32,
        alert_type: AlertType,
        severity: AlertSeverity,
        message: impl Into<String>,
        timestamp: u64,
    ) -> Self {
        Self {
            alert_id,
            sensor_id,
            alert_type,
            severity,
            message: message.into(),
            timestamp,
            acknowledged: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum AlertType {
    HighTemperature,
    LowTemperature,
    StormWarning,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum AlertSeverity {
    Info,
    Warning,
    Critical,
}

// ============================================================================
// Producer Tasks (Write Operations)
// ============================================================================

/// Producer: Read sensor data and write to database
///
/// Receives owned Producer<WeatherReading> handle - no lifetime parameters needed!
/// The writer is internally Arc-based, so it's cheap to clone and pass around.
#[aimdb::task]
async fn store_sensor_data(
    sensor_id: u32,
    writer: Producer<WeatherReading>,
    ctx: RuntimeContext,
) -> DbResult<()> {
    loop {
        // Read from physical sensor hardware (I2C, SPI, etc.)
        let (temperature, humidity, pressure) = read_weather_sensor().await?;
        
        // Get timestamp from runtime context
        let timestamp = ctx.now();
        
        // Create record from sensor data
        let reading = WeatherReading::new(sensor_id, temperature, humidity, pressure, timestamp);
        
        // Write to database - internally uses SPMC buffer
        // All consumers will receive this data
        writer.insert(reading).await?;
        
        // Sleep between readings
        ctx.sleep_millis(1000).await;
    }
}

// ============================================================================
// Consumer Tasks (Read Operations)
// ============================================================================

/// Consumer: Monitor temperature and generate alerts
///
/// Receives owned Consumer<WeatherReading> - no borrowing, no lifetimes!
/// Also receives Producer<WeatherAlert> to insert alerts.
#[aimdb::task]
async fn monitor_temperature(
    reader: Consumer<WeatherReading>,
    alert_writer: Producer<WeatherAlert>,
    ctx: RuntimeContext,
) -> DbResult<()> {
    // Subscribe returns owned values (cloned from buffer)
    let mut stream = reader.subscribe().await;
    
    // Stream yields owned WeatherReading instances
    while let Some(reading) = stream.next().await {
        if reading.temperature > 35.0 {
            let alert = WeatherAlert::new(
                ctx.generate_id(),  // Generate unique ID
                reading.sensor_id,
                AlertType::HighTemperature,
                AlertSeverity::Warning,
                format!("High temperature: {:.1}°C", reading.temperature),
                ctx.now(),
            );
            
            // Insert alert into database
            // Will be picked up by publish_alerts_to_mqtt consumer
            alert_writer.insert(alert).await?;
        }
    }
    
    Ok(())
}

/// Consumer: Publish alerts to MQTT broker
///
/// Receives Consumer<WeatherAlert> and MqttClient handle.
/// Demonstrates explicit MQTT publishing in a dedicated task.
#[aimdb::task]
async fn publish_alerts_to_mqtt(
    reader: Consumer<WeatherAlert>,
    mqtt: MqttClient,
) -> DbResult<()> {
    // Subscribe to all WeatherAlert records
    let mut stream = reader.subscribe().await;
    
    while let Some(alert) = stream.next().await {
        if alert.severity >= AlertSeverity::Warning {
            // Option 1: Use metadata from #[mqtt_message] macro
            // The macro generates topic formatting and serialization
            mqtt.publish(&alert).await?;
            
            // Option 2: Custom topic override for critical alerts
            if alert.severity == AlertSeverity::Critical {
                let custom_topic = format!("weather/critical/{}", alert.sensor_id);
                mqtt.publish_to(custom_topic, &alert).await?;
            }
        }
    }
    
    Ok(())
}

/// Consumer: Log weather statistics
///
/// Demonstrates multiple consumers reading from the same record.
#[aimdb::task]
async fn log_statistics(
    reader: Consumer<WeatherReading>,
    ctx: RuntimeContext,
) -> DbResult<()> {
    let mut stream = reader.subscribe().await;
    let mut count = 0u64;
    let mut temp_sum = 0.0f32;
    
    while let Some(reading) = stream.next().await {
        count += 1;
        temp_sum += reading.temperature;
        
        if count % 10 == 0 {
            let avg_temp = temp_sum / count as f32;
            ctx.log_info(&format!(
                "Sensor {}: {} readings, avg temp: {:.1}°C",
                reading.sensor_id, count, avg_temp
            ));
        }
    }
    
    Ok(())
}

// ============================================================================
// Record Configuration Functions
// ============================================================================

/// Build WeatherReading record with producer and consumers
///
/// The builder pattern handles:
/// 1. Creating the underlying buffer (SPMC ring, SingleLatest, or Mailbox)
/// 2. Spawning producer tasks with Producer<WeatherReading> handles
/// 3. Spawning consumer tasks with Consumer<WeatherReading> handles
/// 4. Connecting cross-record dependencies (alert_writer passed to monitor_temperature)
pub fn weather_reading_record() -> RecordConfig<WeatherReading> {
    RecordConfig::builder()
        // Configure the buffer type and capacity
        .buffer(BufferCfg::SPMCRing { capacity: 100 })
        
        // Add producer - receives Producer<WeatherReading>
        .service(|writer, ctx| {
            // Sensor ID could come from config
            store_sensor_data(1, writer, ctx)
        })
        
        // Add consumers - receive Consumer<WeatherReading>
        .service::<WeatherAlert, _>(|reader, alert_writer, ctx| {
            monitor_temperature(reader, alert_writer, ctx)
        })
        .service(|reader, ctx| {
            log_statistics(reader, ctx)
        })
        
        .build()
}

/// Build WeatherAlert record with MQTT publisher consumer
///
/// Demonstrates connecting to external systems (MQTT broker).
pub fn weather_alert_record() -> RecordConfig<WeatherAlert> {
    RecordConfig::builder()
        // Use SingleLatest buffer - only need most recent alert
        .buffer(BufferCfg::SingleLatest)
        
        // Consumer receives Reader and MqttClient handle
        .link("mqtt://127.0.0.1:1883")
            .out::<WeatherAlert>(|reader, mqtt| {
            publish_alerts_to_mqtt(reader, mqtt)
        })
        .build()
}