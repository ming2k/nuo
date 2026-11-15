use chrono::{Local, TimeZone};

/// The queued item's send time, or `None` when the driver did not report one.
///
/// A missing stamp must not be rendered as *some* time: `sent_time_label(0)` is
/// the Unix epoch in local time (`08:00` in UTC+8), so an `unwrap_or(0)` in the
/// projection painted a fabricated wall-clock next to the user's queued message.
/// Time is either known or it is not shown.
pub fn sent_time_label(epoch_ms: u64) -> String {
    let secs = (epoch_ms / 1000) as i64;
    let nsecs = ((epoch_ms % 1000) * 1_000_000) as u32;
    Local
        .timestamp_opt(secs, nsecs)
        .single()
        .map(|dt| dt.format("%H:%M").to_string())
        .unwrap_or_else(|| "--:--".to_string())
}

/// [`sent_time_label`] for an optional stamp: `None` renders nothing.
pub fn optional_time_label(epoch_ms: Option<u64>) -> Option<String> {
    epoch_ms.map(sent_time_label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sent_time_label_returns_hour_minute() {
        let label = sent_time_label(1_700_000_000_123);
        assert_eq!(label.len(), 5);
        assert_eq!(label.as_bytes()[2], b':');
    }
}
