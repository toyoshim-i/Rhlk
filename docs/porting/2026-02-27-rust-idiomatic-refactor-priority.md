# Rustらしさ監査とリファクタリング優先度 (2026-02-27)

## 目的
`Rhlk` 実装のうち「動くが Rust らしくない」箇所を整理し、保守性と安全性の観点で実施順を固定する。

## 監査方法
- 対象: `Rhlk/src/*.rs`
- 観点:
  - エラー型の明確さ
  - API 設計（引数の型安全）
  - 所有権・clone コスト
  - 関数責務の分離
  - テスト容易性
- 補足:
  - `cargo clippy --all-targets --all-features -- -W clippy::all -W clippy::pedantic` を実行。
  - 実行時点の主な警告は `writer.rs` / `linker.rs` に集中（総計 150+）。

## Clippyサマリ（2026-02-27）
- 高優先で効く警告群:
  - `if_same_then_else` / `needless_bool`（`linker.rs` の `g2lk_mode` 判定）
  - `too_many_lines`（`load_objects_with_requests`, `evaluate_a0`）
  - `ref_option`（`resolve_map_output` の引数型）
  - `manual_find`（`resolve_requested_path`）
- 体系的対応が必要な警告群:
  - 数値 cast 系（`cast_possible_truncation`, `cast_sign_loss`, `cast_possible_wrap`）
  - `match_same_arms`, `unnested_or_patterns`（分岐整理で改善可能）
  - `missing_errors_doc`, `must_use_candidate`（公開 API のドキュメント/属性整備）

## 優先度 P0（先に着手）

### P0-1: 文字列一致ベースのエラー変換を型付きエラーへ置換
- 該当:
  - `Rhlk/src/writer.rs:35-45`
- 問題:
  - `err.to_string().contains("...")` で分岐しており、文言変更で壊れる。
- 改善:
  - `WriterError` などの enum を導入し、`match` で分岐。
  - CLI 向け文言変換は最終層で実施。

### P0-2: 巨大モジュールの責務分離（特に writer）
- 該当:
  - `Rhlk/src/writer.rs`（全 5346 行）
  - `Rhlk/src/writer.rs:2532` 以降に大量 test 同居
- 問題:
  - 1 ファイルに出力生成・式評価・SCD処理・map処理・診断処理が集中。
  - レビュー/変更影響の見通しが悪い。
- 改善:
  - `writer/x.rs`, `writer/r.rs`, `writer/map.rs`, `writer/expr.rs`, `writer/scd.rs` に分割。
  - test も機能別に `Rhlk/tests/*` へ段階移行。

### P0-3: オーケストレーション関数の分割と設定 struct 化
- 該当:
  - `Rhlk/src/linker.rs:12-121` (`run`)
  - `Rhlk/src/writer.rs:10-25` (`write_output` の多引数)
- 問題:
  - bool/数値の並び引数が多く、誤渡しリスクが高い。
  - `run` が入力展開・注入・検証・配置・出力を1関数で処理。
- 改善:
  - `LinkPlan` / `OutputOptions` struct を導入して引数を型で束ねる。
  - `run` を `expand_inputs`, `prepare_objects`, `link_and_emit` に分割。

## 優先度 P1（P0 の次）

### P1-1: 不要 clone と一時 Vec 再構築の削減
- 該当:
  - `Rhlk/src/linker.rs:688-722` (`select_archive_members`)
  - `Rhlk/src/linker.rs:551-553`（archive member 追加時 clone）
  - `Rhlk/src/main.rs:35`（`argv.clone()`）
- 問題:
  - ループ内 `to_vec()/clone()` が多く、可読性と効率を悪化。
- 改善:
  - `select_archive_members` は「現在の defs 集合」を更新する増分アルゴリズムへ変更。
  - 必要な箇所は `Arc`/参照で共有。
  - `main` は前処理結果と解析結果の責務を分けて clone 回避。

### P1-2: `String` ベースのパス API を `Path/PathBuf` 中心へ寄せる
- 該当:
  - `Rhlk/src/linker.rs:123-167`, `345-418`, `447-567`
  - `Rhlk/src/writer.rs:1221-1223`
- 問題:
  - パスを都度 `String` 化しており OS 差異と変換コストが増える。
- 改善:
  - 内部 API は `&Path` / `PathBuf` に統一。
  - ユーザー表示時のみ文字列へ変換。

### P1-3: コマンド走査ロジックの重複統合
- 該当:
  - `Rhlk/src/writer.rs:1343-1411` (`patch_opaque_commands`)
  - `Rhlk/src/writer.rs:1763-1878` (`validate_unsupported_expression_commands`)
- 問題:
  - 同種の `Command` 走査・カーソル更新が複数箇所に散在。
- 改善:
  - 共通の `CommandWalker`（section/cursor/calc_stack を持つ）を導入し、評価/検証/実体化をコールバック化。

## 優先度 P2（改善余地）

### P2-1: `main.rs` の手作業 argv 前処理を専用関数へ
- 該当:
  - `Rhlk/src/main.rs:4-34`
- 問題:
  - オプション互換前処理が main に直書きで拡張しづらい。
- 改善:
  - `cli::normalize_argv` を作ってユニットテスト可能にする。

### P2-2: リテラル値の意味付け強化
- 該当:
  - `Rhlk/src/writer.rs` の多数の opcode 分岐（例: `0x4c01`, `0xe00c`）
- 問題:
  - 意味がコード値に埋め込まれ、追跡が困難。
- 改善:
  - `enum Opcode` / 定数テーブル化し、コメント依存を減らす。

### P2-3: 回帰補助スクリプトの責務分離
- 該当:
  - `tools/run_hlkx_regression.sh`
- 問題:
  - 正規化・実行・比較が単一スクリプトに集中。
- 改善:
  - normalize 処理を関数ファイル分離、将来的に Rust 実装化して OS 非依存化。

## 実施順（推奨）
1. P0-1（エラー型）
2. P0-3（設定 struct + run 分割）
3. P0-2（writer 分割）
4. P1-3（コマンド走査統合）
5. P1-1 / P1-2（clone・path 整理）
6. P2 系

## 受け入れ条件（Refactor 完了判定）
- `cargo test -q --manifest-path Rhlk/Cargo.toml` が全通
- `./tools/run_hlkx_regression.sh` が全 PASS
- 既知差分（map ヘッダパス正規化のみ）に変化なし

## 進捗（2026-02-27 更新）
- P0-1: 完了
  - 文字列 `contains` 判定を廃止し、型付き `WriterError` で分岐。
- P0-2: 完了（第一段）
  - `writer/map.rs` へ map 出力責務を分離。
  - `writer/ctor_dtor.rs` へ ctor/dtor テーブル処理を分離。
- P0-3: 完了
  - `writer::OutputOptions` を導入し `write_output` の多引数を解消。
  - `linker::run` を段階関数へ分割:
    - `validate_args`
    - `expand_inputs`
    - `prepare_objects`
    - `emit_outputs`
    - `validate_start_address_uniqueness`

検証:
- `cargo test -q --manifest-path Rhlk/Cargo.toml`: pass
- `./tools/run_hlkx_regression.sh`: All regression cases matched
