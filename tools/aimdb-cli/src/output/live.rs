//! Live Output Formatting (for watch command)

use chrono::Utc;
use colored::Colorize;

/// Format a subscription update for live display.
///
/// The reshaped AimX-v2 wire drops the server-minted `timestamp`/`dropped`
/// fields, so the watcher stamps the receipt time locally and tracks its own
/// sequence counter; `data` is the decoded record value.
pub fn format_event(seq: u64, data: &serde_json::Value, show_full: bool) -> String {
    let time_str = Utc::now()
        .format("%Y-%m-%d %H:%M:%S%.3f")
        .to_string()
        .dimmed();
    let seq_str = format!("seq:{seq}").cyan();

    let mut output = format!("{time_str} | {seq_str} | ");
    if show_full {
        if let Ok(formatted) = serde_json::to_string_pretty(data) {
            output.push('\n');
            output.push_str(&formatted);
        } else {
            output.push_str(&format!("{data}"));
        }
    } else {
        output.push_str(&format!("{data}"));
    }
    output
}

/// Print a live update to stdout.
pub fn print_event(seq: u64, data: &serde_json::Value, show_full: bool) {
    println!("{}", format_event(seq, data, show_full));
}

/// Print subscription start message
pub fn print_watch_start(record_name: &str) {
    println!("📡 Watching record: {}", record_name.bold());
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
        let data = json!({"temperature": 23.5});
        let formatted = format_event(42, &data, false);
        assert!(formatted.contains("seq:42"));
        assert!(formatted.contains("temperature"));
    }

    #[test]
    fn test_format_event_with_dropped() {
        // AimX-v2 wire does not carry dropped counts; format_event receives
        // only the decoded value. Verify data is still rendered correctly.
        let data = json!({"value": 1});
        let formatted = format_event(42, &data, false);
        assert!(formatted.contains("seq:42"));
        assert!(formatted.contains("value"));
    }
}
