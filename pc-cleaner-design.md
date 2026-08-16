# pc-cleaner — 設計書

Windows を優先しつつ OS 依存を隔離した、手動選択型ディスク掃除ツール。
Rust 製、ゴミ箱削除を既定、判定支援を核とする。

---

## 1. 設計目標

| 目標 | 手段 |
|------|------|
| 死にツール化を防ぐ | 候補に「何を・なぜ・どれだけ」を付与し、判定を丸投げしない |
| 誤削除を防ぐ | ゴミ箱経由を既定、ドライラン既定、許可リスト方式 |
| OS 依存を閉じ込める | `Platform` trait で抽象化、固有実装を1ファイルに隔離 |
| CLI/GUI で挙動を共有 | ロジックを `core` に集約、UI は表示と操作のみ |
| 将来拡張に耐える | 管理者権限領域・他OS を構造を変えず後付け |

---

## 2. ワークスペース構成

```
pc-cleaner/
├── Cargo.toml            # workspace定義
├── core/                 # ライブラリcrate：ロジック本体（UI非依存）
│   ├── rule.rs               # 掃除ルール（許可リスト）
│   ├── entry.rs              # 削除候補エントリ
│   ├── scan.rs               # 走査・容量集計
│   ├── recommend.rs          # 推奨ヒューリスティック（純粋関数）
│   ├── delete.rs             # 削除実行（ゴミ箱/ドライラン）
│   ├── config.rs             # ユーザー設定の保存/読込
│   └── platform/
│       ├── mod.rs            # trait Platform（OS非依存の抽象）
│       ├── windows.rs        # #[cfg(windows)] 実装をここに隔離
│       └── unknown.rs        # 他OS用スタブ（当面最小）
├── cli/                  # バイナリcrate：薄いCLI（検証・自動化用）
└── gui/                  # バイナリcrate：egui（手動選択用、後で追加）
```

`core` は OS 固有 API を直接呼ばない。`cli` と `gui` が `core` を共有する。
CLI で固めた挙動がそのまま GUI に乗る。

---

## 3. プラットフォーム分離

OS 固有の知識は `Platform` trait の裏に閉じ込める。ルール定義には
`C:\Windows\Temp` のような生パスを書かず、**抽象キー**で書く。

```rust
pub trait Platform {
    fn known_dir(&self, kind: KnownDir) -> Option<PathBuf>;
    fn to_trash(&self, path: &Path) -> Result<()>;
    fn requires_admin(&self, path: &Path) -> bool;
}

pub enum KnownDir {
    UserTemp,       // %TEMP%
    SystemTemp,     // C:\Windows\Temp （将来：要管理者）
    LocalAppData,   // %LOCALAPPDATA%
    Cache,          // 各種キャッシュ基点
    RecycleBin,
}
```

- `#[cfg(windows)]` は `platform/windows.rs` の中にだけ登場させる。他に漏らさない。
- パス解決とゴミ箱送りだけが `windows.rs` に入る。ルールとロジックは非依存のまま。
- `trash` クレートはクロスプラットフォーム対応なので `to_trash` は薄い。

---

## 4. データモデル

### 4.1 Rule（掃除ルール＝許可リストの1項目）

判定支援の一次情報。ここに実用性が集約される。

```rust
pub struct Rule {
    pub id: String,           // "windows_temp"（安定した識別子）
    pub label: String,        // 表示名「Windows一時ファイル」
    pub description: String,  // 「アプリが使う一時領域。再作成される」
    pub base: KnownDir,       // 走査の基点（抽象キー）
    pub match_kind: MatchKind,// 絞り込み（拡張子/更新日時/全部）
    pub needs_admin: bool,    // 管理者権限が要るか（当面 true は除外）
    pub safety: Safety,       // Safe / Caution / Review
    pub age_threshold_days: Option<u64>, // 推奨に使う経過日数の既定
}

pub enum MatchKind {
    All,
    Extension(Vec<String>),  // 例：ログ .log .tmp
    OlderThan,               // age_threshold_days と併用
}

pub enum Safety { Safe, Caution, Review }
```

- `description` と `safety` で、ユーザーは個別ファイルを調べずに判断できる。
- 経過日数しきい値はルールにハードコード（初版）。将来 `Config` へ移せる構造。

### 4.2 ScanEntry（走査結果の候補）

「一覧だけでは判定できない」への回答。`age_days`・`recommended`・`reason` が主役。

```rust
pub struct ScanEntry {
    pub rule_id: String,
    pub path: PathBuf,
    pub size: u64,
    pub file_count: u64,
    pub modified: Option<SystemTime>,
    pub age_days: Option<u64>,   // 判定支援の中心
    pub recommended: bool,       // ツールの推奨（初期チェック状態）
    pub reason: String,          // なぜ推奨/非推奨か短文
    pub selected: bool,          // ユーザーの選択状態
}
```

---

## 5. 判定支援ロジック

「一覧を出されても不要と判定しづらい」を解く4段構え。
ユーザーは白紙から選ぶのではなく、**ツールの推奨をレビューして微調整する**立場になる。

1. **ルールが理由を語る** — `description`＋`safety` を判断材料として提示。
2. **安全度で既定選択** — Safe は既定 ON、Caution/Review は既定 OFF。
   起動直後に「Safeだけ選択済み」でワンクリック掃除が可能。
3. **メタ情報の付与** — 最終更新日時・総容量・ファイル数・由来ルール・経過日数を一覧に出す。
4. **推奨の動的補正** — `recommend()` で状況に応じて上書き。

```rust
// 純粋関数。テストしやすく CLI/GUI 共通で効く。
pub fn recommend(entry: &ScanEntry, rule: &Rule) -> Recommendation {
    // 例：使用中の可能性があるTemp（更新が数分前）は推奨から外す
    // 例：180日以上経過のログは推奨ON寄りに
}

pub struct Recommendation {
    pub recommended: bool,
    pub reason: String,
}
```

### 安全度の意味

| Safety | 内容 | 既定 |
|--------|------|------|
| Safe | 再生成される一時領域・キャッシュ | チェック ON |
| Caution | 状況次第（古いDL、大きなログ） | OFF・注意色 |
| Review | 中身の確認が必要 | OFF・確認前提 |

---

## 6. 削除フローの安全設計

必ず **走査 → プレビュー → 実行** の3段を通す。

```rust
pub struct Config {
    pub rule_prefs: HashMap<String, RulePref>, // ルール毎の既定（常に選択/除外/毎回確認）
    pub use_trash: bool,        // 既定 true（ゴミ箱経由）
    pub dry_run_default: bool,  // 既定 true
}
```

- 実行時の既定は `use_trash = true`（`trash` クレートでゴミ箱送り）。
- CLI は明示解除しない限り消さない（`--dry-run` が既定的挙動）。
- 完全削除は明示オプションのときだけ。
- 設定は `%APPDATA%\pc-cleaner\config.json` 等に永続化。2回目以降は即実行できる。

---

## 7. 初期ルール（実用性の初期弾）

確実に安全で効果が見える定番のみ。まず Safe で固め、Caution 以降は後付け。

| id | 対象 | 基点 | Safety |
|----|------|------|--------|
| `user_temp` | ユーザー一時ファイル | UserTemp | Safe |
| `browser_cache` | ブラウザ/アプリのキャッシュ | Cache | Safe |
| `thumbnail_cache` | サムネイルキャッシュ | LocalAppData | Safe |
| `recycle_bin` | ゴミ箱 | RecycleBin | Safe |
| `old_logs` | 古いログ（180日超） | LocalAppData | Caution |
| `old_downloads` | 古いダウンロード（90日超） | — | Review |
| `system_temp` | C:\Windows\Temp | SystemTemp | （将来・要管理者） |

---

## 8. 実装ロードマップ

| 段階 | 内容 | 成果 |
|------|------|------|
| 第1段階 | workspace ＋ `core`（rule/entry/scan/recommend/delete/config/platform） | ロジック本体 |
| 第2段階 | 薄い CLI（`scan` 一覧 / `clean --dry-run` プレビュー / `clean` ゴミ箱削除） | 動く実用ツール |
| 第3段階 | egui GUI（チェックボックス一覧・容量表示・設定保存） | 手動選択 UI |

---

## 9. 将来対応

初版スコープ外の項目は別ファイル `pc-cleaner-roadmap.md` に優先度別で整理。
主な柱は、管理者権限領域（A）、クロスプラットフォーム（B）、判定支援の強化（C）、
安全性・信頼性（D）、UI/配布（E）。いずれも `Platform` trait や `Config` の
構造をそのまま活かして後付けできる設計にしてある。
