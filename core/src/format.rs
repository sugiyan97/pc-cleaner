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

/// 経過秒数を日本語の相対時間表現に変換する（C4 / Issue #50）。
///
/// `chrono` 等の日時クレートは意図的に導入しないため（依存を最小限に保つ
/// 方針。`core/Cargo.toml` の `trash` クレートの features を絞っているのと
/// 同じ理由）、暦日単位の厳密な計算（うるう年・月末等）は行わず、
/// `recommend.rs` の `describe_age` と語彙を揃えた単純な日数バケツ分けで
/// 近似する。用途は履歴一覧の目安表示であり、正確な暦日は要求されない。
///
/// 区分（`seconds_ago` を 86400 で割った日数 `days` による）：
/// - `days == 0` → "本日"
/// - `1..=6` → "N日前"
/// - `7..=29` → "N週間前"（`days / 7`）
/// - `30..=364` → "Nか月前"（`days / 30`）
/// - `365..` → "N年前"（`days / 365`）
pub fn relative_days(seconds_ago: u64) -> String {
    const SECS_PER_DAY: u64 = 24 * 60 * 60;
    let days = seconds_ago / SECS_PER_DAY;

    if days == 0 {
        "本日".to_string()
    } else if days < 7 {
        format!("{days}日前")
    } else if days < 30 {
        format!("{}週間前", days / 7)
    } else if days < 365 {
        format!("{}か月前", days / 30)
    } else {
        format!("{}年前", days / 365)
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

    #[test]
    fn relative_days_formats_common_cases() {
        const DAY: u64 = 24 * 60 * 60;
        assert_eq!(relative_days(0), "本日");
        assert_eq!(relative_days(DAY - 1), "本日");
        assert_eq!(relative_days(DAY), "1日前");
        assert_eq!(relative_days(3 * DAY), "3日前");
        assert_eq!(relative_days(10 * DAY), "1週間前");
        assert_eq!(relative_days(45 * DAY), "1か月前");
        assert_eq!(relative_days(400 * DAY), "1年前");
    }
}
