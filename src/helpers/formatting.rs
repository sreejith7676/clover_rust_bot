use chrono::DateTime;

// Convert 1764007529 -> "November 24, 2025 (06:05 PM UTC)"
pub fn format_timestamp(timestamp: i64) -> String {
    match DateTime::from_timestamp(timestamp, 0) {
        Some(datetime) => datetime.format("%B %d, %Y (%I:%M %p UTC)").to_string(),
        None => "Invalid Date".to_string(),
    }
}

/// "October 2026" for a unix timestamp.
pub fn format_month_year(timestamp: i64) -> Option<String> {
    DateTime::from_timestamp(timestamp, 0).map(|d| d.format("%B %Y").to_string())
}

pub fn format_uptime(seconds: u64) -> String {
    let d = seconds / 86400;
    let h = (seconds % 86400) / 3600;
    let m = (seconds % 3600) / 60;
    let s = seconds % 60;

    if d > 0 {
        format!("{} days, {} hours, {} minutes", d, h, m)
    } else if h > 0 {
        format!("{} hours, {} minutes, {} seconds", h, m, s)
    } else if m > 0 {
        format!("{} minutes, {} seconds", m, s)
    } else {
        format!("{} seconds", s)
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GIB {
        format!("{:.2} GB", b / GIB)
    } else if b >= MIB {
        format!("{:.2} MB", b / MIB)
    } else {
        format!("{} B", bytes)
    }
}

pub fn format_size_value(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                format_bytes(u)
            } else if let Some(f) = n.as_f64() {
                format_bytes(f as u64)
            } else {
                n.to_string()
            }
        }
        serde_json::Value::String(s) => s.clone(),
        _ => "N/A".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_labelled_utc() {
        assert_eq!(format_timestamp(1764007529), "November 24, 2025 (06:05 PM UTC)");
        assert_eq!(format_month_year(1764007529).as_deref(), Some("November 2025"));
    }

    #[test]
    fn uptime_and_sizes() {
        assert_eq!(format_uptime(59), "59 seconds");
        assert_eq!(format_uptime(90_061), "1 days, 1 hours, 1 minutes");
        assert_eq!(format_bytes(3_777_677_544), "3.52 GB");
        assert_eq!(format_size_value(&serde_json::json!(2_600_401_168u64)), "2.42 GB");
        assert_eq!(format_size_value(&serde_json::json!("1 GB")), "1 GB");
        assert_eq!(format_size_value(&serde_json::json!(null)), "N/A");
    }
}
