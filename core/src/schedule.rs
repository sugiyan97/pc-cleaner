//! 定期実行（タスクスケジューラ連携）のスケジュール指定（E2 / Issue #56）。
//!
//! ここに置くのは OS 非依存のデータ型だけである。`schtasks.exe` 呼び出し
//! そのものは Windows 固有の知識なので `platform/windows.rs` 側に閉じ込め、
//! `Platform::install_scheduled_task` 越しにのみ本モジュールの型とやり取りする
//! （要件 4.2 / NF-MNT-03。`platform` モジュールの分離方針と同じ）。
//!
//! [`ScheduleSpec`] が意図的に持たないもの：`all`（Caution/Review を含める
//! かどうか）・`permanent`（完全削除）・`admin`（管理者権限領域）に相当する
//! フィールドは存在しない。定期実行で登録するタスクは常に
//! `pc-cleaner clean --scheduled`（Safe ルールのみ・ゴミ箱経由）に固定され、
//! 無人実行が管理者領域や完全削除に触れる経路をこの型の形自体で塞ぐ
//! （NF-SAF-05）。

/// 定期実行の頻度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleFrequency {
    /// 毎日実行する。
    Daily,
    /// 毎週日曜日に実行する（曜日は固定。選択式にする拡張は将来対応とする）。
    Weekly,
}

/// 定期実行のスケジュール指定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduleSpec {
    /// 実行頻度。
    pub frequency: ScheduleFrequency,
    /// 実行時刻（時、0〜23、ローカルタイム）。
    pub hour: u8,
    /// 実行時刻（分、0〜59）。
    pub minute: u8,
}

impl ScheduleSpec {
    /// `HH:MM` 形式の実行時刻文字列（`schtasks /ST` 用）。
    pub fn start_time(&self) -> String {
        format!("{:02}:{:02}", self.hour, self.minute)
    }
}

/// タスクスケジューラの登録状況（[`crate::platform::Platform::scheduled_task_status`] の戻り値）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleStatus {
    /// タスクが登録されているか。
    pub installed: bool,
    /// 登録済みの場合、`schtasks` から取得した詳細（次回実行時刻等）を
    /// そのまま保持する。取得できなかった場合は `None`。
    pub detail: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_time_is_zero_padded() {
        let spec = ScheduleSpec {
            frequency: ScheduleFrequency::Daily,
            hour: 3,
            minute: 5,
        };
        assert_eq!(spec.start_time(), "03:05");
    }

    #[test]
    fn start_time_handles_double_digit_values() {
        let spec = ScheduleSpec {
            frequency: ScheduleFrequency::Weekly,
            hour: 23,
            minute: 59,
        };
        assert_eq!(spec.start_time(), "23:59");
    }
}
