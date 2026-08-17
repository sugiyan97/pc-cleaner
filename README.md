# pc-cleaner

Windows を優先しつつ OS 依存を隔離した、手動選択型のディスク掃除ツール。Rust 製。

一般的なディスク掃除ツールは削除候補の一覧を提示するだけで「不要か否か」の判定をユーザーに丸投げしがちです。pc-cleaner はこれを避けるため、各候補に「何を・なぜ・どれだけ削除するのか」を必ず付与し、ユーザーが「白紙から選ぶ」のではなく「ツールの推奨をレビューして微調整する」立場になれることを目指しています。

## 設計方針

- **判定支援を核とする**：安全度（Safe / Caution / Review）による既定選択、経過日数・容量などのメタ情報、推奨理由の提示によって、ユーザーの判断を支援する。
- **誤削除を防ぐ**：削除はゴミ箱経由・ドライランを既定とし、対象は許可リスト方式（明示的に定義したルールに合致するもののみ）に限定する。
- **OS 依存を閉じ込める**：OS 固有の知識は `Platform` trait の裏に隠蔽し、固有実装（現状は Windows）を `core/src/platform/windows.rs` の1ファイルに隔離する。
- **CLI/GUI で挙動を共有する**：判定・走査・削除ロジックは UI 非依存の `core` crate に集約し、CLI・GUI はいずれも `core` を呼び出すだけの薄い層とする。

詳細は [`docs/requirements.md`](docs/requirements.md)（要件定義書）を参照してください。初版スコープ外の将来対応は [`docs/roadmap.md`](docs/roadmap.md) にまとめています。

## 現在の状況

初版は Cargo workspace として実装中です。`core` crate（ロジック本体）を中心に、以下が実装済みです。

| モジュール | 内容 |
|---|---|
| `core/src/rule.rs` | 掃除ルール（許可リスト）の型定義と初期ルールセット |
| `core/src/entry.rs` | 走査結果の候補エントリ（`ScanEntry`） |
| `core/src/platform/` | OS 固有知識を隠蔽する `Platform` trait、Windows 実装、他 OS 向け最小スタブ |
| `core/src/scan.rs` | ルールの基点配下を走査し `ScanEntry` を生成する走査機能 |
| `core/src/recommend.rs` | 安全度・経過日数から推奨可否を判定する純粋関数 |
| `core/src/config.rs` | ユーザー設定の JSON 永続化 |
| `core/src/delete.rs` | 「走査 → プレビュー → 実行」を型で強制する削除実行（ゴミ箱送り／ドライラン／完全削除） |

`cli` / `gui` crate は未実装です。進捗は [Issue #2](https://github.com/sugiyan97/pc-cleaner/issues/2)（親 Issue）以下の Sub Issue で管理しています。

## ビルド・テスト

Rust 1.85 以降が必要です（`Cargo.toml` の `rust-version` を参照）。

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

`core` は現状 Windows 固有のゴミ箱送り実装（`trash` クレート）を Windows ターゲット限定の依存として持ちますが、`core` crate 自体は macOS / Linux 上でもビルド・テストできます（`platform/windows.rs` 以外に OS 固有コードは存在しません）。CI（GitHub Actions）は `windows-latest` 上で上記コマンドを実行します。

## ライセンス

未定。
