//! Live Output Formatting (for watch command)

use aimdb_client::Stamp;
use chrono::Utc;
use colored::Colorize;

/// Format a subscription update for live display.
///
/// Shows the local receipt time, the watcher's own sequence counter, the
/// value's write stamp (`ts`, seconds since the server instance started, see
/// [`format_watch_start`]) and the decoded record value.
pub fn format_event(seq: u64, ts: Stamp, data: &serde_json::Value, show_full: bool) -> String {
    let time_str = Utc::now()
        .format("%Y-%m-%d %H:%M:%S%.3f")
        .to_string()
        .dimmed();
    let seq_str = format!("seq:{seq}").cyan();
    let ts_str = format!("ts:{}", super::format_ts(ts)).dimmed();

    let mut output = format!("{time_str} | {seq_str} | {ts_str} | ");
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

/// Format a delivery-gap notice: `skipped` updates were lost between the
/// previous printed event and the next one.
pub fn format_gap(skipped: u64) -> String {
    let plural = if skipped == 1 { "update" } else { "updates" };
    format!("⚠️  gap: {skipped} {plural} dropped before the next event")
        .yellow()
        .to_string()
}

/// Format an undecodable-payload notice: the frame arrived and is accounted
/// for, but its payload was not JSON the client could decode.
pub fn format_undecodable(seq: u64) -> String {
    let seq_str = format!("seq:{seq}").cyan();
    format!("{seq_str} | ⚠️  payload did not decode (malformed server frame)")
        .yellow()
        .to_string()
}

/// Format the end of a late-join snapshot burst: every matched record's current
/// value has now been delivered, so what follows is live events.
pub fn format_snapshot_complete(records: u64, lost: u64) -> String {
    let plural = if records == 1 { "record" } else { "records" };
    let mut line = format!("📸 snapshot complete ({records} {plural}");
    if lost > 0 {
        line.push_str(&format!(", {lost} lost"));
    }
    line.push(')');
    line.dimmed().to_string()
}

/// Format the subscription start message.
pub fn format_watch_start(record_name: &str) -> String {
    format!(
        "📡 Watching record: {}\n{}\n{}\n",
        record_name.bold(),
        "ts: time since instance start, in seconds".dimmed(),
        "Press Ctrl+C to stop".dimmed()
    )
}

/// Format the subscription stop message, reporting the total updates lost
/// across the session (`0` stays silent — the common lossless case).
pub fn format_watch_stop(skipped_total: u64) -> String {
    let mut out = String::from("\n");
    if skipped_total > 0 {
        out.push_str(
            &format!("⚠️  {skipped_total} update(s) dropped during this session\n")
                .yellow()
                .to_string(),
        );
    }
    out.push_str(&"✅ Stopped watching".green().to_string());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_format_event() {
        let data = json!({"temperature": 23.5});
        let formatted = format_event(42, Stamp::from_nanos(81_234_567_890), &data, false);
        assert!(formatted.contains("seq:42"));
        assert!(formatted.contains("ts:81.234567"), "{formatted}");
        assert!(formatted.contains("temperature"));
    }

    #[test]
    fn test_format_gap() {
        assert!(format_gap(1).contains("1 update dropped"));
        assert!(format_gap(7).contains("7 updates dropped"));
    }

    #[test]
    fn test_format_undecodable() {
        let formatted = format_undecodable(9);
        assert!(formatted.contains("seq:9"));
        assert!(formatted.contains("did not decode"));
    }

    #[test]
    fn test_format_snapshot_complete() {
        // The lossless case stays quiet about loss.
        let clean = format_snapshot_complete(3, 0);
        assert!(clean.contains("3 records"));
        assert!(!clean.contains("lost"));
        // A truncated burst reports what it cost.
        let lossy = format_snapshot_complete(1, 12);
        assert!(lossy.contains("1 record"));
        assert!(lossy.contains("12 lost"));
    }

    #[test]
    fn test_format_event_with_dropped() {
        // AimX wire does not carry dropped counts; format_event receives
        // only the decoded value. Verify data is still rendered correctly.
        let data = json!({"value": 1});
        let formatted = format_event(42, Stamp::from_nanos(0), &data, false);
        assert!(formatted.contains("seq:42"));
        assert!(formatted.contains("value"));
    }
}
