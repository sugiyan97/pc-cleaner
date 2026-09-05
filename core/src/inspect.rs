//! 走査結果に対する追加調査（使用中判定・重複検出）。C2 / Issue #48。
//!
//! いずれも走査フェーズと [`crate::recommend::recommend`] の間に挟まる
//! 前処理であり、I/O を伴う判定を [`crate::recommend::recommend`] の外へ
//! 追い出すことで、その純粋関数契約（NF-MNT-02 / F-REC-05）を保つ。

use crate::entry::{DuplicateInfo, ScanEntry};
use crate::platform::Platform;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;

/// ハッシュ計算の対象とする最大ファイルサイズ（バイト）。
///
/// これを超えるファイルは部分的な読み取りで済ませず、丸ごとスキップする。
/// 途中までしか読まずに「同一」と判定すると、後半が異なるファイルを
/// 誤って重複と報告しかねないため（安全側に倒す。NF-SAF-01）。
const MAX_HASH_BYTES: u64 = 64 * 1024 * 1024;

/// 単一ファイル（`file_count == 1`）のエントリについて
/// `Platform::is_file_in_use` を呼び、`entry.in_use` を埋める。
///
/// 集約されたディレクトリ（`file_count != 1`）は対象にしない：どの構成
/// ファイルを指して「使用中」と言うべきか一意に決まらないため、`None` の
/// ままにしておく方が誤解を招かない。
pub fn annotate_in_use(platform: &dyn Platform, entries: &mut [ScanEntry]) {
    for entry in entries {
        if entry.file_count != 1 {
            continue;
        }
        entry.in_use = platform.is_file_in_use(&entry.path);
    }
}

/// 単一ファイルのエントリのうち、サイズが一致するものだけを内容ハッシュで
/// 突き合わせ、2件以上が一致したグループに `entry.duplicate` を設定する。
///
/// 手順：
/// 1. `file_count == 1` かつ `0 < size <= MAX_HASH_BYTES` のエントリだけを
///    候補にする（`0` バイトのファイルは中身が無く「重複」と呼んでも
///    情報価値が無いため除外する）。
/// 2. まずサイズだけでバケット分けする（安価・I/O 不要）。
/// 3. 2件以上のバケットに対してのみファイルを読み、内容ハッシュ
///    （`twox-hash`）を計算する。
/// 4. `(size, hash)` が一致するグループのうち2件以上のものに
///    `DuplicateInfo` を設定する。`is_primary` は `age_days` が最大
///    （最も古い）のエントリ。同点の場合はパス順（昇順）で先頭を採用する。
///
/// あくまで情報提供であり、`recommend()` の推奨可否を変えることはない。
pub fn annotate_duplicates(entries: &mut [ScanEntry]) {
    let mut by_size: HashMap<u64, Vec<usize>> = HashMap::new();
    for (index, entry) in entries.iter().enumerate() {
        if entry.file_count == 1 && entry.size > 0 && entry.size <= MAX_HASH_BYTES {
            by_size.entry(entry.size).or_default().push(index);
        }
    }

    let mut next_group_id = 0u64;

    for (_size, indices) in by_size {
        if indices.len() < 2 {
            continue;
        }

        let mut by_hash: HashMap<u64, Vec<usize>> = HashMap::new();
        for index in indices {
            let Some(hash) = hash_file(&entries[index].path) else {
                continue;
            };
            by_hash.entry(hash).or_default().push(index);
        }

        for (_hash, mut group_indices) in by_hash {
            if group_indices.len() < 2 {
                continue;
            }
            // パス順で決定的にする（9.5 と同じ考え方）。
            group_indices.sort_by(|&a, &b| entries[a].path.cmp(&entries[b].path));

            let primary_index = *group_indices
                .iter()
                .max_by_key(|&&i| (entries[i].age_days, std::cmp::Reverse(&entries[i].path)))
                .unwrap();

            let group_id = next_group_id;
            next_group_id += 1;
            let group_size = group_indices.len();

            for index in group_indices {
                entries[index].duplicate = Some(DuplicateInfo {
                    group_id,
                    group_size,
                    is_primary: index == primary_index,
                });
            }
        }
    }
}

/// ファイル内容全体を読み、`twox-hash`（XXH3, 64bit）でハッシュ化する。
/// 読み取りに失敗した場合は `None`（走査全体を失敗させない。NF-SAF-01）。
fn hash_file(path: &std::path::Path) -> Option<u64> {
    use std::hash::Hasher;
    use twox_hash::XxHash3_64;

    let mut file = File::open(path).ok()?;
    let mut hasher = XxHash3_64::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.write(&buf[..n]);
    }
    Some(hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{ElevateResult, KnownDir, PlatformError};
    use std::fs;
    use std::path::{Path, PathBuf};

    struct FakePlatform {
        in_use: std::collections::HashSet<PathBuf>,
    }

    impl FakePlatform {
        fn new() -> Self {
            FakePlatform {
                in_use: std::collections::HashSet::new(),
            }
        }
    }

    impl Platform for FakePlatform {
        fn known_dir(&self, _kind: KnownDir) -> Option<PathBuf> {
            None
        }

        fn to_trash(&self, _path: &Path) -> crate::platform::Result<()> {
            Err(PlatformError::Unsupported("to_trash"))
        }

        fn requires_admin(&self, _path: &Path) -> bool {
            false
        }

        fn config_dir(&self) -> Option<PathBuf> {
            None
        }

        fn is_elevated(&self) -> bool {
            false
        }

        fn elevate(&self, _args: &[String]) -> ElevateResult {
            Err(crate::platform::ElevateError::Unsupported)
        }

        fn is_file_in_use(&self, path: &Path) -> Option<bool> {
            Some(self.in_use.contains(path))
        }
    }

    fn entry(path: &str, size: u64, file_count: u64, age_days: Option<u64>) -> ScanEntry {
        ScanEntry {
            rule_id: "test".to_string(),
            path: PathBuf::from(path),
            size,
            file_count,
            modified: None,
            age_days,
            in_use: None,
            duplicate: None,
            recommended: false,
            reason: String::new(),
            selected: false,
        }
    }

    fn write_file(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    // ---- annotate_in_use ----

    #[test]
    fn probe_reports_not_in_use_for_a_plain_writable_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        write_file(&file, b"hello");

        let platform = crate::platform::current();
        let mut entries = vec![entry(file.to_str().unwrap(), 5, 1, Some(0))];
        annotate_in_use(platform.as_ref(), &mut entries);

        assert_eq!(entries[0].in_use, Some(false));
    }

    #[test]
    fn probe_returns_none_for_a_missing_path() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("does-not-exist.txt");

        let platform = crate::platform::current();
        let mut entries = vec![entry(file.to_str().unwrap(), 0, 1, Some(0))];
        annotate_in_use(platform.as_ref(), &mut entries);

        assert_eq!(entries[0].in_use, None);
    }

    #[test]
    fn annotate_in_use_skips_aggregated_directory_entries() {
        let platform = FakePlatform::new();
        let mut entries = vec![entry("/some/dir", 100, 3, Some(0))];
        annotate_in_use(&platform, &mut entries);
        assert_eq!(entries[0].in_use, None);
    }

    // ---- annotate_duplicates ----

    #[test]
    fn duplicates_require_identical_content_not_just_size() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        write_file(&a, b"AAAAA");
        write_file(&b, b"BBBBB"); // 同じサイズ・異なる内容

        let mut entries = vec![
            entry(a.to_str().unwrap(), 5, 1, Some(10)),
            entry(b.to_str().unwrap(), 5, 1, Some(20)),
        ];
        annotate_duplicates(&mut entries);

        assert!(entries[0].duplicate.is_none());
        assert!(entries[1].duplicate.is_none());
    }

    #[test]
    fn duplicates_group_three_identical_files_with_one_primary() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        let c = dir.path().join("c.bin");
        write_file(&a, b"same content");
        write_file(&b, b"same content");
        write_file(&c, b"same content");

        let mut entries = vec![
            entry(a.to_str().unwrap(), 12, 1, Some(5)),
            entry(b.to_str().unwrap(), 12, 1, Some(50)), // 最古 -> primary
            entry(c.to_str().unwrap(), 12, 1, Some(1)),
        ];
        annotate_duplicates(&mut entries);

        for e in &entries {
            let info = e.duplicate.expect("グループに属するはず");
            assert_eq!(info.group_size, 3);
        }
        assert_eq!(
            entries[0].duplicate.unwrap().group_id,
            entries[1].duplicate.unwrap().group_id
        );
        assert_eq!(
            entries[1].duplicate.unwrap().group_id,
            entries[2].duplicate.unwrap().group_id
        );

        assert!(!entries[0].duplicate.unwrap().is_primary);
        assert!(
            entries[1].duplicate.unwrap().is_primary,
            "最も古いものが primary"
        );
        assert!(!entries[2].duplicate.unwrap().is_primary);
    }

    #[test]
    fn files_over_the_hash_cap_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        write_file(&a, b"x");
        write_file(&b, b"x");

        let too_big = MAX_HASH_BYTES + 1;
        let mut entries = vec![
            entry(a.to_str().unwrap(), too_big, 1, Some(1)),
            entry(b.to_str().unwrap(), too_big, 1, Some(2)),
        ];
        annotate_duplicates(&mut entries);

        assert!(entries[0].duplicate.is_none());
        assert!(entries[1].duplicate.is_none());
    }

    #[test]
    fn zero_byte_files_are_not_grouped() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        write_file(&a, b"");
        write_file(&b, b"");

        let mut entries = vec![
            entry(a.to_str().unwrap(), 0, 1, Some(1)),
            entry(b.to_str().unwrap(), 0, 1, Some(2)),
        ];
        annotate_duplicates(&mut entries);

        assert!(entries[0].duplicate.is_none());
        assert!(entries[1].duplicate.is_none());
    }

    #[test]
    fn directory_aggregate_entries_are_not_considered_for_duplicates() {
        let mut entries = vec![
            entry("/dir/a", 10, 2, Some(1)),
            entry("/dir/b", 10, 2, Some(1)),
        ];
        annotate_duplicates(&mut entries);
        assert!(entries[0].duplicate.is_none());
        assert!(entries[1].duplicate.is_none());
    }
}
