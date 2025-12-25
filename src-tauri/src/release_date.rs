use chrono::{Utc, TimeZone};

pub fn parse_release_date_to_timestamp(releasedate: &str) -> Option<i64> {
    if releasedate.is_empty() {
        return None;
    }

    let releasedate = releasedate.trim();

    // Try common formats
    // 1. "24 December 2025" or "24 Dec 2025"
    if let Ok(dt) = chrono::NaiveDate::parse_from_str(releasedate, "%d %B %Y") {
        return Some(Utc.from_utc_datetime(&dt.and_hms_opt(0, 0, 0)?).timestamp());
    }
    if let Ok(dt) = chrono::NaiveDate::parse_from_str(releasedate, "%d %b %Y") {
        return Some(Utc.from_utc_datetime(&dt.and_hms_opt(0, 0, 0)?).timestamp());
    }
    
    // 2. "December 2025" or "Dec 2025" (Assume 1st of month)
    if let Ok(dt) = chrono::NaiveDate::parse_from_str(&format!("1 {}", releasedate), "%d %B %Y") {
        return Some(Utc.from_utc_datetime(&dt.and_hms_opt(0, 0, 0)?).timestamp());
    }
    if let Ok(dt) = chrono::NaiveDate::parse_from_str(&format!("1 {}", releasedate), "%d %b %Y") {
        return Some(Utc.from_utc_datetime(&dt.and_hms_opt(0, 0, 0)?).timestamp());
    }

    // 3. "2025" (Year only - Assume Jan 1st)
    if let Ok(year) = releasedate.parse::<i32>() {
        if year > 1900 && year < 2100 {
            let dt = chrono::NaiveDate::from_ymd_opt(year, 1, 1)?;
            return Some(Utc.from_utc_datetime(&dt.and_hms_opt(0, 0, 0)?).timestamp());
        }
    }

    None
}

pub fn compute_ttl_seconds(now_ts: i64, release_ts: Option<i64>) -> i64 {
    let release_ts = match release_ts {
        Some(ts) => ts,
        None => return 180 * 86400, // Default to 180 days if unknown
    };

    let age_seconds = now_ts - release_ts;
    let age_days = age_seconds / 86400;

    if age_days < 14 {
        1 * 86400 // 1 day
    } else if age_days < 30 {
        3 * 86400 // 3 days
    } else if age_days < 180 { // 6 months
        14 * 86400 // 14 days
    } else if age_days < 365 { // 1 year
        30 * 86400 // 30 days
    } else if age_days < 730 { // 2 years
        90 * 86400 // 90 days
    } else {
        180 * 86400 // 180 days
    }
}

pub fn is_fresh(fetched_at: i64, ttl_seconds: i64, now_ts: i64) -> bool {
    (now_ts - fetched_at) < ttl_seconds
}
