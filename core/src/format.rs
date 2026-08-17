//! 表示用フォーマッタ。CLI/GUI 共通の整形ロジックであり、判定ロジックでは
//! ない（F-CLI-01 / F-GUI-07）。CLI/GUI が個別に同じ実装を持つと将来ズレる
//! ため、ここに集約する。

/// バイト数を読みやすい単位（B/KB/MB/GB/TB）に変換する。
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit_index = 0;
    while size >= 1024.0 && unit_index < UNITS.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }
    if unit_index == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{size:.1} {}", UNITS[unit_index])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_size_formats_common_magnitudes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(1024 * 1024 * 1024), "1.0 GB");
    }
}
