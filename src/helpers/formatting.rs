use chrono::DateTime;

// Convert 1764007529 -> "November 24, 2025 (06:05 PM)"
pub fn format_timestamp(timestamp: i64) -> String {
    match DateTime::from_timestamp(timestamp, 0) {
        Some(datetime) => datetime.format("%B %d, %Y (%I:%M %p)").to_string(),
        None => "Invalid Date".to_string(),
    }
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
