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
