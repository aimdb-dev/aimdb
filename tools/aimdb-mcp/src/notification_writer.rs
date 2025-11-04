//! Notification file writer
//!
//! Writes MCP notifications to per-record JSONL files for LLM consumption.
//! Files are organized as: {base_dir}/{date}__{record_name}.jsonl

use crate::protocol::Notification;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::{self, OpenOptions};
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::sync::Mutex;
use tracing::{debug, error, warn};

/// Notification file writer
///
/// Manages file handles for notification persistence. Creates one file per
/// record per day in JSONL format for easy LLM consumption.
#[derive(Clone)]
pub struct NotificationFileWriter {
    /// Base directory for notification files
    base_path: PathBuf,
    /// Cached file handles by "date__record" key
    file_cache: Arc<Mutex<HashMap<String, BufWriter<fs::File>>>>,
    /// Whether file writing is enabled
    enabled: bool,
}

/// Notification entry written to file
#[derive(Debug, serde::Serialize)]
struct NotificationEntry {
    /// Server-side timestamp (Unix millis)
    timestamp: i64,
    /// Notification value
    value: Value,
    /// Sequence number for this subscription
    #[serde(skip_serializing_if = "Option::is_none")]
    sequence: Option<u64>,
}

impl NotificationFileWriter {
    /// Create a new notification file writer
    ///
    /// # Arguments
    /// * `base_path` - Directory to store notification files
    /// * `enabled` - Whether to actually write files
    pub async fn new(base_path: PathBuf, enabled: bool) -> Result<Self, std::io::Error> {
        if enabled {
            // Create base directory if it doesn't exist
            fs::create_dir_all(&base_path).await?;
            debug!(
                "📁 Notification file writer initialized at {}",
                base_path.display()
            );
        } else {
            debug!("📁 Notification file writer disabled");
        }

        Ok(Self {
            base_path,
            file_cache: Arc::new(Mutex::new(HashMap::new())),
            enabled,
        })
    }

    /// Write a notification to the appropriate file
    ///
    /// Extracts record information from the notification and appends it to
    /// the corresponding JSONL file.
    pub async fn write(&self, notification: &Notification) -> Result<(), std::io::Error> {
        if !self.enabled {
            return Ok(());
        }

        // Extract record information from notification
        let (record_name, value, timestamp, sequence) =
            match extract_notification_data(notification) {
                Some(data) => data,
                None => {
                    warn!("⚠️  Failed to extract record data from notification");
                    return Ok(()); // Don't fail the event loop
                }
            };

        // Get current date for file naming
        let date = chrono::Utc::now().format("%Y-%m-%d").to_string();

        // Sanitize record name for safe filename
        let sanitized_record = sanitize_record_name(&record_name);

        // Build file key and path
        let file_key = format!("{}__{}", date, sanitized_record);
        let filename = format!("{}.jsonl", file_key);
        let file_path = self.base_path.join(&filename);

        // Get or create file handle
        let mut cache = self.file_cache.lock().await;
        let writer = match cache.get_mut(&file_key) {
            Some(writer) => writer,
            None => {
                // Open file in append mode (create if doesn't exist)
                let file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&file_path)
                    .await
                    .map_err(|e| {
                        error!(
                            "Failed to open notification file {}: {}",
                            file_path.display(),
                            e
                        );
                        e
                    })?;

                debug!(
                    "📝 Opened notification file: {} for record {}",
                    file_path.display(),
                    record_name
                );

                cache.insert(file_key.clone(), BufWriter::new(file));
                cache.get_mut(&file_key).unwrap()
            }
        };

        // Create entry
        let entry = NotificationEntry {
            timestamp,
            value,
            sequence,
        };

        // Serialize and write
        let line = serde_json::to_string(&entry).map_err(|e| {
            error!("Failed to serialize notification entry: {}", e);
            std::io::Error::new(std::io::ErrorKind::InvalidData, e)
        })?;

        writer.write_all(line.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;

        debug!(
            "✅ Wrote notification for {} to {}",
            record_name,
            file_path.display()
        );

        Ok(())
    }

    /// Flush all cached file handles
    ///
    /// Ensures all buffered data is written to disk. Should be called
    /// periodically or on shutdown.
    pub async fn flush_all(&self) -> Result<(), std::io::Error> {
        if !self.enabled {
            return Ok(());
        }

        let mut cache = self.file_cache.lock().await;
        for (key, writer) in cache.iter_mut() {
            if let Err(e) = writer.flush().await {
                error!("Failed to flush file {}: {}", key, e);
                return Err(e);
            }
        }

        debug!("💾 Flushed {} notification files", cache.len());
        Ok(())
    }

    /// Close and remove old file handles
    ///
    /// Closes file handles for files not matching the current date.
    /// This prevents accumulating file handles over time.
    pub async fn cleanup_old_handles(&self) {
        if !self.enabled {
            return;
        }

        let current_date = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let mut cache = self.file_cache.lock().await;

        // Remove entries not matching current date
        let old_keys: Vec<String> = cache
            .keys()
            .filter(|key| !key.starts_with(&current_date))
            .cloned()
            .collect();

        for key in old_keys {
            cache.remove(&key);
            debug!("🧹 Closed old notification file: {}", key);
        }

        debug!("🧹 Cleaned up old file handles, {} active", cache.len());
    }
}

/// Extract record data from MCP notification
///
/// Returns (record_name, value, timestamp, sequence) if successful
fn extract_notification_data(
    notification: &Notification,
) -> Option<(String, Value, i64, Option<u64>)> {
    let params = notification.params.as_ref()?;

    let record_name = params.get("record_name")?.as_str()?.to_string();
    let value = params.get("value")?.clone();
    let timestamp = params.get("timestamp")?.as_i64()?;
    let sequence = params.get("sequence").and_then(|s| s.as_u64());

    Some((record_name, value, timestamp, sequence))
}

/// Sanitize record name for safe filesystem usage
///
/// Converts "server::Temperature" to "server__Temperature" and removes
/// any characters that could cause issues in filenames.
fn sanitize_record_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-' => c,
            ':' => '_', // Keep :: -> __ pattern
            _ => '_',
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_sanitize_record_name() {
        assert_eq!(
            sanitize_record_name("server::Temperature"),
            "server__Temperature"
        );
        assert_eq!(sanitize_record_name("foo/bar"), "foo_bar");
        assert_eq!(sanitize_record_name("test.record"), "test_record");
        assert_eq!(sanitize_record_name("valid_name-123"), "valid_name-123");
    }

    #[tokio::test]
    async fn test_writer_creation_disabled() {
        let writer = NotificationFileWriter::new(PathBuf::from("/tmp/test"), false)
            .await
            .unwrap();
        assert!(!writer.enabled);
    }

    #[tokio::test]
    async fn test_extract_notification_data() {
        let notification = Notification::new(
            "notifications/resources/updated",
            Some(json!({
                "record_name": "server::Temperature",
                "value": {"celsius": 25.0},
                "timestamp": 1234567890,
                "sequence": 42
            })),
        );

        let result = extract_notification_data(&notification);
        assert!(result.is_some());

        let (record_name, _value, timestamp, sequence) = result.unwrap();
        assert_eq!(record_name, "server::Temperature");
        assert_eq!(timestamp, 1234567890);
        assert_eq!(sequence, Some(42));
    }

    #[tokio::test]
    async fn test_extract_notification_data_no_sequence() {
        let notification = Notification::new(
            "notifications/resources/updated",
            Some(json!({
                "record_name": "server::Status",
                "value": {"status": "ok"},
                "timestamp": 1234567890
            })),
        );

        let result = extract_notification_data(&notification);
        assert!(result.is_some());

        let (_record_name, _value, _timestamp, sequence) = result.unwrap();
        assert_eq!(sequence, None);
    }
}
