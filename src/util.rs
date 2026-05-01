use std::time::{Duration, SystemTime};

use mtp_rs::ptp::DateTime;

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn format_mtp_datetime(datetime: Option<DateTime>) -> String {
    datetime
        .map(|dt| {
            format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second
            )
        })
        .unwrap_or_else(|| "--".to_string())
}

pub fn mtp_datetime_to_system_time(datetime: Option<DateTime>) -> SystemTime {
    datetime
        .and_then(|dt| {
            let days = days_from_civil(dt.year as i32, dt.month as u32, dt.day as u32)?;
            let seconds = days
                .checked_mul(86_400)?
                .checked_add(dt.hour as i64 * 3_600)?
                .checked_add(dt.minute as i64 * 60)?
                .checked_add(dt.second as i64)?;
            u64::try_from(seconds).ok()
        })
        .map(|seconds| SystemTime::UNIX_EPOCH + Duration::from_secs(seconds))
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

fn days_from_civil(year: i32, month: u32, day: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let year = year - i32::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = (year - era * 400) as u32;
    let month = month as i32;
    let day = day as i32;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era =
        year_of_era as i32 * 365 + year_of_era as i32 / 4 - year_of_era as i32 / 100 + day_of_year;
    Some((era * 146_097 + day_of_era - 719_468) as i64)
}

pub fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '\0' => '_',
            _ => ch,
        })
        .collect();
    if cleaned.is_empty() {
        "preview.bin".to_string()
    } else {
        cleaned
    }
}

pub fn format_mtp_error(err: &mtp_rs::Error) -> String {
    let message = err.to_string();
    if err.is_exclusive_access() {
        format!(
            "{message}\n\nmacOS 的 ptpcamerad 或 Android File Transfer 可能占用了设备。请退出相关程序，必要时临时运行: pkill -9 ptpcamerad"
        )
    } else {
        message
    }
}
