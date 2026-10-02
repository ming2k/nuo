use chrono::{Local, TimeZone};

pub(crate) fn sent_time_label(epoch_ms: u64) -> String {
    let secs = (epoch_ms / 1000) as i64;
    let nsecs = ((epoch_ms % 1000) * 1_000_000) as u32;
    Local
        .timestamp_opt(secs, nsecs)
        .single()
        .map(|dt| dt.format("%H:%M").to_string())
        .unwrap_or_else(|| "--:--".to_string())
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
