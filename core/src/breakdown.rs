//! 削除対象の内訳集計（種類別・容量別）。C3「プレビュー時の内訳表示」。
//!
//! GUI は表示と操作のみを担い判定・集計ロジックを持たないため（F-GUI-07）、
//! 内訳の算出（グルーピング・合計・並び替え）はすべてここに置く。`gui` 側
//! （`view.rs`）はこの集計結果を文字列やバーの比率に変換するだけの、
//! `egui` 非依存の表示整形しか行わない。

use crate::delete::DeletePlan;
use crate::entry::ScanEntry;
use serde::Serialize;

/// 内訳集計の入力単位。`ScanEntry` / `PlannedDeletion` のどちらからも
/// 変換できるよう、集計に必要な最小限のフィールドだけを持つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakdownItem {
    /// 由来ルールの `Rule::id`。
    pub rule_id: String,
    /// サイズ（バイト）。
    pub size: u64,
    /// ファイル数。
    pub file_count: u64,
}

/// ルール（種類）別の内訳1件。
///
/// `Serialize` はエクスポート機能（`export.rs` / Issue #52）が JSON 出力に
/// 埋め込むために使う（出力専用のため `Deserialize` は持たせない）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CategoryBreakdown {
    /// 由来ルールの `Rule::id`。
    pub rule_id: String,
    /// このルールに属する項目数。
    pub item_count: usize,
    /// このルールに属する合計ファイル数。
    pub file_count: u64,
    /// このルールに属する合計サイズ（バイト）。
    pub total_size: u64,
}

/// サイズ帯の区分。境界は下限を含み上限を含まない（`[lower, upper)`）。
///
/// `Serialize` は `CategoryBreakdown` と同じ理由（`export.rs` / Issue #52）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SizeBucket {
    /// 1MB 未満。
    UnderMib,
    /// 1MB 以上 10MB 未満。
    Mib1To10,
    /// 10MB 以上 100MB 未満。
    Mib10To100,
    /// 100MB 以上 1GB 未満。
    Mib100ToGib,
    /// 1GB 以上。
    OverGib,
}

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

impl SizeBucket {
    /// 表示用の日本語ラベル。
    pub fn label(&self) -> &'static str {
        match self {
            SizeBucket::UnderMib => "1MB未満",
            SizeBucket::Mib1To10 => "1MB〜10MB",
            SizeBucket::Mib10To100 => "10MB〜100MB",
            SizeBucket::Mib100ToGib => "100MB〜1GB",
            SizeBucket::OverGib => "1GB以上",
        }
    }

    /// 固定の表示順（`by_size_bucket` が返す順序と一致する）。
    pub fn all() -> [SizeBucket; 5] {
        [
            SizeBucket::UnderMib,
            SizeBucket::Mib1To10,
            SizeBucket::Mib10To100,
            SizeBucket::Mib100ToGib,
            SizeBucket::OverGib,
        ]
    }

    fn for_size(size: u64) -> Self {
        if size < MIB {
            SizeBucket::UnderMib
        } else if size < 10 * MIB {
            SizeBucket::Mib1To10
        } else if size < 100 * MIB {
            SizeBucket::Mib10To100
        } else if size < GIB {
            SizeBucket::Mib100ToGib
        } else {
            SizeBucket::OverGib
        }
    }
}

/// サイズ帯別の内訳1件。
///
/// `Serialize` は `CategoryBreakdown` と同じ理由（`export.rs` / Issue #52）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BucketBreakdown {
    /// サイズ帯。
    pub bucket: SizeBucket,
    /// この帯に属する項目数。
    pub item_count: usize,
    /// この帯に属する合計サイズ（バイト）。
    pub total_size: u64,
}

/// 選択中（`ScanEntry::selected == true`）のエントリから内訳集計の入力を作る。
pub fn from_selected_entries(entries: &[ScanEntry]) -> Vec<BreakdownItem> {
    entries
        .iter()
        .filter(|e| e.selected)
        .map(|e| BreakdownItem {
            rule_id: e.rule_id.clone(),
            size: e.size,
            file_count: e.file_count,
        })
        .collect()
}

/// 削除計画（`DeletePlan::items()`）から内訳集計の入力を作る。
pub fn from_plan(plan: &DeletePlan) -> Vec<BreakdownItem> {
    plan.items()
        .iter()
        .map(|item| BreakdownItem {
            rule_id: item.rule_id.clone(),
            size: item.size,
            file_count: item.file_count,
        })
        .collect()
}

/// `rule_id` ごとに集計する。合計サイズの降順、同着は `rule_id` の昇順で
/// 安定した並びにする（表示・テストの決定性のため）。
pub fn by_rule(items: &[BreakdownItem]) -> Vec<CategoryBreakdown> {
    let mut order: Vec<String> = Vec::new();
    let mut acc: std::collections::HashMap<String, (usize, u64, u64)> =
        std::collections::HashMap::new();

    for item in items {
        let entry = acc.entry(item.rule_id.clone()).or_insert_with(|| {
            order.push(item.rule_id.clone());
            (0, 0, 0)
        });
        entry.0 += 1;
        entry.1 += item.file_count;
        entry.2 += item.size;
    }

    let mut result: Vec<CategoryBreakdown> = order
        .into_iter()
        .map(|rule_id| {
            let (item_count, file_count, total_size) = acc.remove(&rule_id).unwrap_or_default();
            CategoryBreakdown {
                rule_id,
                item_count,
                file_count,
                total_size,
            }
        })
        .collect();

    result.sort_by(|a, b| {
        b.total_size
            .cmp(&a.total_size)
            .then_with(|| a.rule_id.cmp(&b.rule_id))
    });
    result
}

/// サイズ帯ごとに集計する。空の帯も含め、固定順（[`SizeBucket::all`]）で
/// すべて返す（合計が常に完全であることをテストしやすくするため）。
pub fn by_size_bucket(items: &[BreakdownItem]) -> Vec<BucketBreakdown> {
    let mut counts = [0usize; 5];
    let mut sizes = [0u64; 5];

    for item in items {
        let bucket = SizeBucket::for_size(item.size);
        let index = SizeBucket::all().iter().position(|b| *b == bucket).unwrap();
        counts[index] += 1;
        sizes[index] += item.size;
    }

    SizeBucket::all()
        .into_iter()
        .enumerate()
        .map(|(i, bucket)| BucketBreakdown {
            bucket,
            item_count: counts[i],
            total_size: sizes[i],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(rule_id: &str, size: u64) -> BreakdownItem {
        BreakdownItem {
            rule_id: rule_id.to_string(),
            size,
            file_count: 1,
        }
    }

    #[test]
    fn by_rule_sums_and_sorts_by_size_desc() {
        let items = vec![item("a", 10), item("b", 100), item("a", 20), item("c", 50)];
        let result = by_rule(&items);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].rule_id, "b");
        assert_eq!(result[0].total_size, 100);
        assert_eq!(result[1].rule_id, "c");
        assert_eq!(result[1].total_size, 50);
        assert_eq!(result[2].rule_id, "a");
        assert_eq!(result[2].total_size, 30);
        assert_eq!(result[2].item_count, 2);
        assert_eq!(result[2].file_count, 2);
    }

    #[test]
    fn by_rule_breaks_ties_by_rule_id() {
        let items = vec![item("z", 10), item("a", 10), item("m", 10)];
        let result = by_rule(&items);
        let ids: Vec<&str> = result.iter().map(|c| c.rule_id.as_str()).collect();
        assert_eq!(ids, vec!["a", "m", "z"]);
    }

    #[test]
    fn by_rule_handles_empty_input() {
        assert!(by_rule(&[]).is_empty());
    }

    #[test]
    fn by_size_bucket_assigns_boundary_values() {
        let items = vec![
            item("r", MIB - 1),   // UnderMib
            item("r", MIB),       // Mib1To10 (下限含む)
            item("r", 10 * MIB),  // Mib10To100 (下限含む)
            item("r", 100 * MIB), // Mib100ToGib (下限含む)
            item("r", GIB),       // OverGib (下限含む)
            item("r", GIB - 1),   // Mib100ToGib (上限含まない)
        ];
        let buckets = by_size_bucket(&items);
        assert_eq!(buckets.len(), 5);
        assert_eq!(buckets[0].bucket, SizeBucket::UnderMib);
        assert_eq!(buckets[0].item_count, 1);
        assert_eq!(buckets[1].bucket, SizeBucket::Mib1To10);
        assert_eq!(buckets[1].item_count, 1);
        assert_eq!(buckets[2].bucket, SizeBucket::Mib10To100);
        assert_eq!(buckets[2].item_count, 1);
        assert_eq!(buckets[3].bucket, SizeBucket::Mib100ToGib);
        assert_eq!(buckets[3].item_count, 2);
        assert_eq!(buckets[4].bucket, SizeBucket::OverGib);
        assert_eq!(buckets[4].item_count, 1);
    }

    #[test]
    fn by_size_bucket_includes_empty_buckets() {
        let items = vec![item("r", 10)];
        let buckets = by_size_bucket(&items);
        assert_eq!(buckets.len(), 5);
        assert_eq!(buckets[0].item_count, 1);
        for bucket in &buckets[1..] {
            assert_eq!(bucket.item_count, 0);
            assert_eq!(bucket.total_size, 0);
        }
    }

    #[test]
    fn bucket_totals_equal_category_totals() {
        let items = vec![
            item("a", 10),
            item("b", MIB + 5),
            item("a", 100 * MIB),
            item("c", GIB + 1),
        ];
        let category_total: u64 = by_rule(&items).iter().map(|c| c.total_size).sum();
        let bucket_total: u64 = by_size_bucket(&items).iter().map(|b| b.total_size).sum();
        assert_eq!(category_total, bucket_total);
    }
}
