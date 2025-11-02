//! Live Output Formatting (for watch command)

use crate::client::protocol::Event;
use chrono::{DateTime, Utc};
use colored::Colorize;

/// Format an event for live display
pub fn format_event(event: &Event, show_full: bool) -> String {
    let mut output = String::new();

    // Parse timestamp
    let timestamp = parse_timestamp(&event.timestamp);
    let time_str = timestamp
        .format("%Y-%m-%d %H:%M:%S%.3f")
        .to_string()
        .dimmed();

    // Format sequence
    let seq_str = format!("seq:{}", event.sequence).cyan();

    // Build status line
    output.push_str(&format!("{} | {} | ", time_str, seq_str));

    // Show dropped events warning if any
    if let Some(dropped) = event.dropped {
        let warning = format!("⚠️  {} events dropped | ", dropped).yellow();
        output.push_str(&warning.to_string());
    }

    // Format data
    if show_full {
        // Pretty print full JSON
        if let Ok(formatted) = serde_json::to_string_pretty(&event.data) {
            output.push('\n');
            output.push_str(&formatted);
        } else {
            output.push_str(&format!("{}", event.data));
        }
    } else {
        // Compact single-line JSON
        output.push_str(&format!("{}", event.data));
    }

    output
}

/// Parse timestamp string to DateTime
/// Expects format: seconds.nanoseconds (e.g., "1730379296.123456789")
fn parse_timestamp(timestamp_str: &str) -> DateTime<Utc> {
    // Try parsing as floating point (seconds.nanoseconds)
    if let Ok(secs_f64) = timestamp_str.parse::<f64>() {
        let secs = secs_f64.trunc() as i64;
        let nsec = ((secs_f64.fract() * 1_000_000_000.0).round()) as u32;
        if let Some(dt) = DateTime::from_timestamp(secs, nsec) {
            return dt;
        }
    }

    // Fallback to now if parsing fails
    Utc::now()
}

/// Print a live event to stdout
pub fn print_event(event: &Event, show_full: bool) {
    println!("{}", format_event(event, show_full));
}

/// Print subscription start message
pub fn print_watch_start(record_name: &str, subscription_id: &str) {
    println!(
        "📡 Watching record: {} (subscription: {})",
        record_name.bold(),
        subscription_id.dimmed()
    );
    println!("{}", "Press Ctrl+C to stop".dimmed());
    println!();
}

/// Print subscription stop message
pub fn print_watch_stop() {
    println!();
    println!("{}", "✅ Stopped watching".green());
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_format_event() {
        let event = Event {
            subscription_id: "sub123".to_string(),
            sequence: 42,
            timestamp: "1730379296".to_string(),
            data: json!({"temperature": 23.5}),
            dropped: None,
        };

        let formatted = format_event(&event, false);
        assert!(formatted.contains("seq:42"));
        assert!(formatted.contains("temperature"));
    }

    #[test]
    fn test_format_event_with_dropped() {
        let event = Event {
            subscription_id: "sub123".to_string(),
            sequence: 42,
            timestamp: "1730379296".to_string(),
            data: json!({"value": 1}),
            dropped: Some(5),
        };

        let formatted = format_event(&event, false);
        assert!(formatted.contains("5 events dropped"));
    }
}
