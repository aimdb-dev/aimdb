//! Integration tests for notification file writer

use aimdb_mcp::protocol::Notification;
use aimdb_mcp::NotificationFileWriter;
use serde_json::json;
use tempfile::TempDir;
use tokio::fs;

#[tokio::test]
async fn test_notification_writer_creates_directory() {
    let temp_dir = TempDir::new().unwrap();
    let notification_dir = temp_dir.path().join("notifications");

    let writer = NotificationFileWriter::new(notification_dir.clone(), true)
        .await
        .unwrap();

    assert!(notification_dir.exists());

    drop(writer);
}

#[tokio::test]
async fn test_notification_writer_writes_file() {
    let temp_dir = TempDir::new().unwrap();
    let notification_dir = temp_dir.path().join("notifications");

    let writer = NotificationFileWriter::new(notification_dir.clone(), true)
        .await
        .unwrap();

    // Create a notification
    let notification = Notification::new(
        "notifications/resources/updated",
        Some(json!({
            "record_name": "server::Temperature",
            "value": {"celsius": 25.0},
            "timestamp": 1234567890,
            "sequence": 1
        })),
    );

    writer.write(&notification).await.unwrap();

    // Check that file was created
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let filename = format!("{date}__server__Temperature.jsonl");
    let file_path = notification_dir.join(&filename);

    assert!(file_path.exists());

    // Read file content
    let content = fs::read_to_string(&file_path).await.unwrap();
    assert!(content.contains("\"celsius\":25.0"));
    assert!(content.contains("\"timestamp\":1234567890"));
    assert!(content.contains("\"sequence\":1"));

    drop(writer);
}

#[tokio::test]
async fn test_notification_writer_appends_to_same_file() {
    let temp_dir = TempDir::new().unwrap();
    let notification_dir = temp_dir.path().join("notifications");

    let writer = NotificationFileWriter::new(notification_dir.clone(), true)
        .await
        .unwrap();

    // Write first notification
    let notification1 = Notification::new(
        "notifications/resources/updated",
        Some(json!({
            "record_name": "server::Temperature",
            "value": {"celsius": 25.0},
            "timestamp": 1234567890,
            "sequence": 1
        })),
    );
    writer.write(&notification1).await.unwrap();

    // Write second notification
    let notification2 = Notification::new(
        "notifications/resources/updated",
        Some(json!({
            "record_name": "server::Temperature",
            "value": {"celsius": 26.0},
            "timestamp": 1234567891,
            "sequence": 2
        })),
    );
    writer.write(&notification2).await.unwrap();

    // Check file has both entries
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let filename = format!("{date}__server__Temperature.jsonl");
    let file_path = notification_dir.join(&filename);

    let content = fs::read_to_string(&file_path).await.unwrap();
    let lines: Vec<&str> = content.lines().collect();

    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("\"celsius\":25.0"));
    assert!(lines[1].contains("\"celsius\":26.0"));

    drop(writer);
}

#[tokio::test]
async fn test_notification_writer_separate_files_per_record() {
    let temp_dir = TempDir::new().unwrap();
    let notification_dir = temp_dir.path().join("notifications");

    let writer = NotificationFileWriter::new(notification_dir.clone(), true)
        .await
        .unwrap();

    // Write notification for Temperature
    let notification1 = Notification::new(
        "notifications/resources/updated",
        Some(json!({
            "record_name": "server::Temperature",
            "value": {"celsius": 25.0},
            "timestamp": 1234567890,
            "sequence": 1
        })),
    );
    writer.write(&notification1).await.unwrap();

    // Write notification for Pressure
    let notification2 = Notification::new(
        "notifications/resources/updated",
        Some(json!({
            "record_name": "server::Pressure",
            "value": {"pascal": 101325.0},
            "timestamp": 1234567891,
            "sequence": 1
        })),
    );
    writer.write(&notification2).await.unwrap();

    // Check both files exist
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let temp_file = notification_dir.join(format!("{date}__server__Temperature.jsonl"));
    let pressure_file = notification_dir.join(format!("{date}__server__Pressure.jsonl"));

    assert!(temp_file.exists());
    assert!(pressure_file.exists());

    // Verify contents are independent
    let temp_content = fs::read_to_string(&temp_file).await.unwrap();
    let pressure_content = fs::read_to_string(&pressure_file).await.unwrap();

    assert!(temp_content.contains("celsius"));
    assert!(!temp_content.contains("pascal"));

    assert!(pressure_content.contains("pascal"));
    assert!(!pressure_content.contains("celsius"));

    drop(writer);
}

#[tokio::test]
async fn test_notification_writer_disabled() {
    let temp_dir = TempDir::new().unwrap();
    let notification_dir = temp_dir.path().join("notifications");

    let writer = NotificationFileWriter::new(notification_dir.clone(), false)
        .await
        .unwrap();

    // Write notification (should be no-op)
    let notification = Notification::new(
        "notifications/resources/updated",
        Some(json!({
            "record_name": "server::Temperature",
            "value": {"celsius": 25.0},
            "timestamp": 1234567890,
            "sequence": 1
        })),
    );
    writer.write(&notification).await.unwrap();

    // No files should be created
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let filename = format!("{date}__server__Temperature.jsonl");
    let file_path = notification_dir.join(&filename);

    assert!(!file_path.exists());

    drop(writer);
}

#[tokio::test]
async fn test_notification_writer_sanitizes_record_names() {
    let temp_dir = TempDir::new().unwrap();
    let notification_dir = temp_dir.path().join("notifications");

    let writer = NotificationFileWriter::new(notification_dir.clone(), true)
        .await
        .unwrap();

    // Write notification with special characters in record name
    let notification = Notification::new(
        "notifications/resources/updated",
        Some(json!({
            "record_name": "weird/name::with.dots",
            "value": {"test": true},
            "timestamp": 1234567890,
            "sequence": 1
        })),
    );
    writer.write(&notification).await.unwrap();

    // Check that file was created with sanitized name
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let filename = format!("{date}__weird_name__with_dots.jsonl");
    let file_path = notification_dir.join(&filename);

    assert!(file_path.exists());

    drop(writer);
}

#[tokio::test]
async fn test_notification_writer_handles_missing_fields() {
    let temp_dir = TempDir::new().unwrap();
    let notification_dir = temp_dir.path().join("notifications");

    let writer = NotificationFileWriter::new(notification_dir.clone(), true)
        .await
        .unwrap();

    // Notification without sequence field
    let notification = Notification::new(
        "notifications/resources/updated",
        Some(json!({
            "record_name": "server::Temperature",
            "value": {"celsius": 25.0},
            "timestamp": 1234567890
        })),
    );

    // Should succeed (sequence is optional)
    writer.write(&notification).await.unwrap();

    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let filename = format!("{date}__server__Temperature.jsonl");
    let file_path = notification_dir.join(&filename);

    let content = fs::read_to_string(&file_path).await.unwrap();
    // Sequence field should not be present in output
    assert!(!content.contains("\"sequence\""));

    drop(writer);
}

#[tokio::test]
async fn test_notification_writer_flush_all() {
    let temp_dir = TempDir::new().unwrap();
    let notification_dir = temp_dir.path().join("notifications");

    let writer = NotificationFileWriter::new(notification_dir.clone(), true)
        .await
        .unwrap();

    // Write multiple notifications
    for i in 0..5 {
        let notification = Notification::new(
            "notifications/resources/updated",
            Some(json!({
                "record_name": "server::Temperature",
                "value": {"celsius": 20.0 + i as f64},
                "timestamp": 1234567890 + i,
                "sequence": i
            })),
        );
        writer.write(&notification).await.unwrap();
    }

    // Flush all
    writer.flush_all().await.unwrap();

    // Verify all data is written
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let filename = format!("{date}__server__Temperature.jsonl");
    let file_path = notification_dir.join(&filename);

    let content = fs::read_to_string(&file_path).await.unwrap();
    let lines: Vec<&str> = content.lines().collect();

    assert_eq!(lines.len(), 5);

    drop(writer);
}
