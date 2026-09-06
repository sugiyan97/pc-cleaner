# pc-cleaner

Windows を優先しつつ OS 依存を隔離した、手動選択型のディスク掃除ツール。Rust 製。

一般的なディスク掃除ツールは削除候補の一覧を提示するだけで「不要か否か」の判定をユーザーに丸投げしがちです。pc-cleaner はこれを避けるため、各候補に「何を・なぜ・どれだけ削除するのか」を必ず付与し、ユーザーが「白紙から選ぶ」のではなく「ツールの推奨をレビューして微調整する」立場になれることを目指しています。

## 設計方針

- **判定支援を核とする**：安全度（Safe / Caution / Review）による既定選択、経過日数・容量などのメタ情報、推奨理由の提示によって、ユーザーの判断を支援する。
- **誤削除を防ぐ**：削除はゴミ箱経由・ドライランを既定とし、対象は許可リスト方式（明示的に定義したルールに合致するもののみ）に限定する。
- **OS 依存を閉じ込める**：OS 固有の知識は `Platform` trait の裏に隠蔽し、固有実装（現状は Windows）を `core/src/platform/windows.rs` の1ファイルに隔離する。
- **CLI/GUI で挙動を共有する**：判定・走査・削除ロジックは UI 非依存の `core` crate に集約し、CLI・GUI はいずれも `core` を呼び出すだけの薄い層とする。

詳細は [`docs/requirements.md`](docs/requirements.md)（要件定義書）を参照してください。初版スコープ外の将来対応は同書「8. 将来拡張要件」にまとめており、個別の項目は [Issues](https://github.com/sugiyan97/pc-cleaner/issues) で管理しています。

## インストール（Windows）

Rust のビルド環境は不要です。[Releases](https://github.com/sugiyan97/pc-cleaner/releases) ページから最新版の実行ファイルをダウンロードし、そのまま実行してください（インストーラなし、単一の `.exe`）。

- `pc-cleaner-<tag>-windows-x86_64.exe` … CLI（検証・自動化用）
- `pc-cleaner-gui-<tag>-windows-x86_64.exe` … GUI（手動選択 UI。通常はこちらを使う）

署名は行っていないため、初回起動時に Windows Defender SmartScreen の警告が出ることがあります。ソースからビルドしたい場合、または Windows 以外の環境で `core` を使う場合は以下の「開発」を参照してください。

## 開発

以下はソースからビルドする場合や、コントリビュートする場合の情報です。実行ファイルを使うだけであれば上記の「インストール」を参照してください。

### 現在の状況

初版は Cargo workspace として実装済みです。`core` crate（ロジック本体）を中心に、以下が実装済みです。

| モジュール | 内容 |
|---|---|
| `core/src/rule.rs` | 掃除ルール（許可リスト）の型定義と初期ルールセット |
| `core/src/ruleset.rs` | ルール定義ファイル（`rules.json`）による組み込みルールの上書き・追加ルールの検証・解決 |
| `core/src/entry.rs` | 走査結果の候補エントリ（`ScanEntry`） |
| `core/src/platform/` | OS 固有知識を隠蔽する `Platform` trait、Windows 実装、他 OS 向け最小スタブ |
| `core/src/scan.rs` | ルールの基点配下を走査し `ScanEntry` を生成する走査機能 |
| `core/src/recommend.rs` | 安全度・経過日数から推奨可否を判定する純粋関数 |
| `core/src/config.rs` | ユーザー設定の JSON 永続化 |
| `core/src/delete.rs` | 「走査 → プレビュー → 実行」を型で強制する削除実行（ゴミ箱送り／ドライラン／完全削除） |
| `core/src/format.rs` | CLI/GUI 共通の表示フォーマッタ（`human_size` 等） |

### ソースから実行する

薄い CLI（`cli` crate、実行バイナリ名 `pc-cleaner`）が実装済みです。

```sh
pc-cleaner scan               # Safe ルールを走査して一覧表示（削除しない）
pc-cleaner scan --all         # Caution / Review も含めて走査
pc-cleaner clean --dry-run    # 削除予定のプレビューのみ表示
pc-cleaner clean              # ゴミ箱経由で削除を実行
pc-cleaner clean --permanent  # 確認の上、完全削除（復旧不可）
pc-cleaner scan --admin       # 管理者権限が必要な領域も対象にする（UAC の確認を経て管理者として起動し直す）
pc-cleaner log                # 削除ログ（いつ何を削除したか）を表示する
pc-cleaner log --run <RUN_ID> # 指定した実行分のみ表示する
pc-cleaner scan --format json                       # 走査結果を JSON で標準出力へ
pc-cleaner clean --dry-run --format csv --output plan.csv  # 削除予定を CSV でファイルへ書き出す
```

`log` は `clean` の実行結果を記録した削除ログ（`<config_dir>/deletion_log.jsonl`）を表示するだけで、何も削除しません。誤削除が起きた際の追跡用に、実行のたびにパス付きで項目単位の記録を残します（既定で有効。無効化する場合は `config.json` の `audit_log_enabled` を `false` にするか、GUI の「削除ログを記録する」チェックボックスを外してください）。

`--format json|csv` はレビューや自動化連携向けに走査結果・削除計画を機械可読な形で出力するだけで、削除の実行可否そのものは変えません（`--format json` を付けても `clean` は通常どおり削除を実行します。プレビューだけが欲しい場合は `--dry-run` と併用してください）。`--output` を省略すると標準出力に出力し、その際は自動化のパイプを壊さないよう一覧やサマリなどの人間向け出力は表示しません。

`--admin` は未昇格の場合、UAC の確認画面を経て管理者権限で起動し直し、元のプロセスは終了します。`ShellExecuteW` で新しいコンソールウィンドウが開くため、出力は新しいウィンドウに表示され、処理完了と同時に閉じます。出力を確認したい場合は、あらかじめ管理者としてターミナルを開いてから `pc-cleaner` を実行してください（この場合 `--admin` は不要です）。昇格すると、管理者権限が必要な領域（`system_temp` = `C:\Windows\Temp`）も走査・削除の対象になります。ただし既定では選択されず（Safety: Review）、内容を確認したうえで手動で選択する必要があります。

#### ルール定義ファイル（`rules.json`）

`<config_dir>/rules.json` を置くと、組み込みルールの一部設定を上書きしたり、独自のルールを追加したりできます（無効化する場合は `config.json` の `user_rules_enabled` を `false` にするか、GUI の「ルール定義ファイルを読み込む」チェックボックスを外してください）。

```json
{
  "schema_version": 1,
  "overrides": {
    "old_logs": { "age_threshold_days": 365 }
  },
  "rules": [
    {
      "id": "my_app_cache",
      "label": "MyApp のキャッシュ",
      "description": "MyApp が再生成する一時データです。",
      "base": "LocalAppData",
      "match": { "kind": "Extension", "extensions": ["tmp", "cache"] },
      "safety": "Caution"
    }
  ]
}
```

安全のため、次の制約があります（ファイルからこれらを変更しようとした記述は無視され、`pc-cleaner scan`/`clean` 実行時に警告として表示されます）。

- `needs_admin` はファイルから変更できません（管理者権限領域を無確認で対象化させないため）。
- `base` に指定できるのは `UserTemp` / `LocalAppData` / `Cache` / `Downloads` / `ThumbnailCache` のみです。管理者権限領域やゴミ箱は指定できません。
- `overrides` の `safety` はリスクを下げる方向（例: `Review` → `Safe`）には変更できません。ユーザー定義ルール（`rules`）は `safety: Safe` を指定できません（既定 `Review`、`Caution` まで昇格可）。
- ユーザー定義ルールは最大32件までです。

ファイルが存在しない・壊れている場合は組み込みルールのみで動作し、個別のルール記述に誤りがある場合もそのルールだけを無視して続行します。

egui による GUI（`gui` crate、実行バイナリ名 `pc-cleaner-gui`）も実装済みです。起動直後に Safe ルールを走査し、チェックボックス一覧・容量表示・設定保存を行います。

```sh
cargo run -p pc-cleaner-gui
```

非Windows環境では `Platform::known_dir` が常に `None` を返すため走査結果は常に空になります。実装の目視確認用に、一時ディレクトリ配下だけで完結するサンプルデータを使う `demo` feature を用意しています（ユーザーの実ファイルには一切触れません）。

```sh
cargo run -p pc-cleaner-gui --features demo -- --demo
```

初版実装は [Issue #2](https://github.com/sugiyan97/pc-cleaner/issues/2)（親 Issue）以下の Sub Issue で管理し、完了済みです。以降の不具合対応・将来対応は [Issues](https://github.com/sugiyan97/pc-cleaner/issues) で管理しています。

### ビルド・テスト

Rust 1.85 以降が必要です（`Cargo.toml` の `rust-version` を参照）。

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

`core` は現状 Windows 固有のゴミ箱送り実装（`trash` クレート）を Windows ターゲット限定の依存として持ちますが、`core` crate 自体は macOS / Linux 上でもビルド・テストできます（`platform/windows.rs` 以外に OS 固有コードは存在しません）。CI（GitHub Actions）は `windows-latest` 上で上記コマンドを実行します。

### macOS からの Windows 向けクロスビルド（ローカル動作確認用）

Windows 実機を持たない場合でも、`.exe` を手元でビルドすることは可能です。

```sh
brew install mingw-w64
rustup target add x86_64-pc-windows-gnu
scripts/build-windows.sh
```

`dist/windows-local/` に `pc-cleaner-*.exe` / `pc-cleaner-gui-*.exe` が出力されます。生成した `.exe` を Windows 実機・VM に転送すれば動作確認できます。

注意点：

- 公式リリース（`.github/workflows/release.yml`）は `windows-latest` 上で **msvc** ターゲット向けにビルドしています。ここで作るのは **gnu** ターゲット（mingw-w64）向けのビルドで、配布物と完全に同一のバイナリではありません（基本的な動作は同等ですが、リンカ・ランタイムが異なります）。
- リンカ設定は `.cargo/config.toml` の `[target.x86_64-pc-windows-gnu]` に定義済みです。他のターゲットやネイティブ Windows / CI のビルドには影響しません。

### リリース手順（メンテナ向け）

`v*.*.*` 形式の Git タグ（例：`v0.1.0`）を push すると、`.github/workflows/release.yml` が Windows 向けリリースビルドを作成し、GitHub Release として公開する（上記「インストール」の配布物はここで作られる。署名・インストーラは対象外。`docs/requirements.md`「8. 将来拡張要件」E3 の初版範囲）。

タグの値は `Cargo.toml` の `[workspace.package] version` と一致している必要があり、一致しない場合はワークフローが失敗する。リリース手順：

1. `Cargo.toml` の `[workspace.package] version` を更新するコミットを作成する
2. `git tag v<version>` でタグを付け、`git push origin v<version>` で push する
3. Actions が `pc-cleaner-<tag>-windows-x86_64.exe` / `pc-cleaner-gui-<tag>-windows-x86_64.exe` をビルドし、GitHub Release に添付する（リリースノートは自動生成）

## ライセンス

[MIT](LICENSE)
