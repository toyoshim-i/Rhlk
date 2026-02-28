# Rustらしさ監査とリファクタリング優先度 (2026-02-27)

## 目的
`Rhlk` 実装のうち「動くが Rust らしくない」箇所を整理し、保守性と安全性の観点で実施順を固定する。

## 監査方法
- 対象: `src/*.rs`
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
  - `src/writer.rs:35-45`
- 問題:
  - `err.to_string().contains("...")` で分岐しており、文言変更で壊れる。
- 改善:
  - `WriterError` などの enum を導入し、`match` で分岐。
  - CLI 向け文言変換は最終層で実施。

### P0-2: 巨大モジュールの責務分離（特に writer）
- 該当:
  - `src/writer.rs`（全 5346 行）
  - `src/writer.rs:2532` 以降に大量 test 同居
- 問題:
  - 1 ファイルに出力生成・式評価・SCD処理・map処理・診断処理が集中。
  - レビュー/変更影響の見通しが悪い。
- 改善:
  - `writer/x.rs`, `writer/r.rs`, `writer/map.rs`, `writer/expr.rs`, `writer/scd.rs` に分割。
  - test も機能別に `tests/*` へ段階移行。

### P0-3: オーケストレーション関数の分割と設定 struct 化
- 該当:
  - `src/linker.rs:12-121` (`run`)
  - `src/writer.rs:10-25` (`write_output` の多引数)
- 問題:
  - bool/数値の並び引数が多く、誤渡しリスクが高い。
  - `run` が入力展開・注入・検証・配置・出力を1関数で処理。
- 改善:
  - `LinkPlan` / `OutputOptions` struct を導入して引数を型で束ねる。
  - `run` を `expand_inputs`, `prepare_objects`, `link_and_emit` に分割。

## 優先度 P1（P0 の次）

### P1-1: 不要 clone と一時 Vec 再構築の削減
- 該当:
  - `src/linker.rs:688-722` (`select_archive_members`)
  - `src/linker.rs:551-553`（archive member 追加時 clone）
  - `src/main.rs:35`（`argv.clone()`）
- 問題:
  - ループ内 `to_vec()/clone()` が多く、可読性と効率を悪化。
- 改善:
  - `select_archive_members` は「現在の defs 集合」を更新する増分アルゴリズムへ変更。
  - 必要な箇所は `Arc`/参照で共有。
  - `main` は前処理結果と解析結果の責務を分けて clone 回避。

### P1-2: `String` ベースのパス API を `Path/PathBuf` 中心へ寄せる
- 該当:
  - `src/linker.rs:123-167`, `345-418`, `447-567`
  - `src/writer.rs:1221-1223`
- 問題:
  - パスを都度 `String` 化しており OS 差異と変換コストが増える。
- 改善:
  - 内部 API は `&Path` / `PathBuf` に統一。
  - ユーザー表示時のみ文字列へ変換。

### P1-3: コマンド走査ロジックの重複統合
- 該当:
  - `src/writer.rs:1343-1411` (`patch_opaque_commands`)
  - `src/writer.rs:1763-1878` (`validate_unsupported_expression_commands`)
- 問題:
  - 同種の `Command` 走査・カーソル更新が複数箇所に散在。
- 改善:
  - 共通の `CommandWalker`（section/cursor/calc_stack を持つ）を導入し、評価/検証/実体化をコールバック化。

## 優先度 P2（改善余地）

### P2-1: `main.rs` の手作業 argv 前処理を専用関数へ
- 該当:
  - `src/main.rs:4-34`
- 問題:
  - オプション互換前処理が main に直書きで拡張しづらい。
- 改善:
  - `cli::normalize_argv` を作ってユニットテスト可能にする。

### P2-2: リテラル値の意味付け強化
- 該当:
  - `src/writer.rs` の多数の opcode 分岐（例: `0x4c01`, `0xe00c`）
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
  - 実行/比較処理を `tools/lib/regression_case.sh` に分離し、メインスクリプトをオーケストレーション中心に整理。

## 優先度 P3（2026-02-28 再監査で追加）

### P3-1: `linker.rs` の入力ロード補助整理
- 該当:
  - `src/linker.rs:load_objects_with_requests`
  - `src/linker.rs:resolve_requested_path`
- 問題:
  - 単一関数に責務が集中し、エラー文言組み立てが重複。
  - Clippy の `too_many_lines` / `manual_find` が残る。
- 改善:
  - 共通処理（verbose表示・request enqueue・push）を `add_loaded_object` に抽出。
  - 表示名組み立てを `display_name` に抽出。
  - 候補検索を `Iterator::find` 化。

### P3-2: `writer.rs` の符号/桁あふれ cast 集中地帯の明示化
- 該当:
  - `src/writer.rs` の `patch_opaque_commands` 周辺
- 問題:
  - `as` による縮小変換が多く、意図の明確化が必要。
- 改善:
  - 範囲チェック済み変換 helper を導入し、危険な cast を集約。
  - `u32::from`, `i32::cast_unsigned` など意図表現を統一。

### P3-3: `format/obj.rs` の opcode 判定テーブル整理
- 該当:
  - `src/format/obj.rs` の `has_payload` / `payload_size_from_code`
- 問題:
  - `match_same_arms` が多く、条件の意図が読み取りづらい。
- 改善:
  - opcode分類定数・小関数を導入して分岐を簡潔化。

## 実施順（推奨）
1. P0-1（エラー型）
2. P0-3（設定 struct + run 分割）
3. P0-2（writer 分割）
4. P1-3（コマンド走査統合）
5. P1-1 / P1-2（clone・path 整理）
6. P2 系

## 受け入れ条件（Refactor 完了判定）
- `cargo test -q --manifest-path Cargo.toml` が全通
- `./tools/run_hlkx_regression.sh` が全 PASS
- 既知差分（map ヘッダパス正規化のみ）に変化なし

## 進捗（2026-02-27 更新）
- P0-1: 完了
  - 文字列 `contains` 判定を廃止し、型付き `WriterError` で分岐。
- P0-2: 完了（第一段）
  - `writer/map.rs` へ map 出力責務を分離。
  - `writer/ctor_dtor.rs` へ ctor/dtor テーブル処理を分離。
  - `writer/expr.rs` を新設し、式評価・式由来エラー分類ロジックを `writer.rs` から分離。
- P0-3: 完了
  - `writer::OutputOptions` を導入し `write_output` の多引数を解消。
  - `linker::run` を段階関数へ分割:
    - `validate_args`
    - `expand_inputs`
    - `prepare_objects`
    - `emit_outputs`
    - `validate_start_address_uniqueness`
- P3-1: 完了
  - `load_objects_with_requests` の共通処理を `add_loaded_object` / `display_name` へ抽出。
  - `resolve_requested_path` を `Iterator::find` 化。
  - object 読み込み状態を `LoadState` struct に統合し、`add_loaded_object` の多引数 helper（`too_many_arguments` 許可）を廃止。
- P3-2: 完了
  - `writer.rs` の `patch_opaque` 系で数値変換 helper（`code_hi/lo`, `i32_low_u8/u16`, bit-cast helper）を導入。
  - `label_no as u32` 等を `u32::from(...)` に置換し、意図しない縮小 cast を削減。
  - `patch_opaque_commands` を `Result<()>` から副作用関数へ整理し、不要な `?` を除去。
  - `resolve_exec_address` / 診断処理まわりで `as` 縮小変換、`single_match`, `if_not_else` を解消。
  - テストコードを `std::slice::from_ref` に統一し、不要 clone を削減。
  - `build_x_header` を `XHeader` struct 引数化して多引数警告を解消。
  - `main.rs` / `cli.rs` の `similar_names` を命名修正で解消。
  - `writer.rs` の `u32 -> i64` / `usize -> u32` 変換を `From` / helper 経由へ統一。
  - `writer/expr.rs` の式演算分岐を整理し、符号付き/符号なし変換を `cast_unsigned/cast_signed` ベースへ統一。
  - `writer/map.rs` の文字列生成を `writeln!` 化し、`format_push_string` を解消。
  - `resolver.rs` に `#[must_use]` 付与と `usize -> u32` 変換 helper を適用。
  - `linker.rs` のフォーマット/closure 警告を解消し、内部 helper に `too_many_arguments` 許可を明示。
  - `cli.rs` / `format/obj.rs` / `layout.rs` / `linker.rs` の pedantic 警告を追加で解消し、`clippy` を全体クリーン化。
  - `writer.rs` 内の巨大 `mod tests` を `writer/tests.rs` へ分離し、本体ロジックとテスト責務を分離。
  - `OutputOptions` の bool 群を enum 化（`OutputFormat`, `RelocationCheck`, `BssPolicy`, `SymbolTablePolicy`）し、`linker` 側も型安全に接続。
  - `Args` に型付き派生状態（`G2lkMode`, `OutputRequest`）を導入し、`linker` の分岐を bool 直参照から enum ベースへ移行。
  - `Args::runtime_config()` を追加し、`linker::run` の実行時分岐を `RuntimeConfig` 経由に一本化。
- P3-3: 完了
  - `format/obj.rs` の opaque opcode 判定分岐を整理し、重複 arm を統合。
  - `is_label_section` を range pattern 化、`align_even` を `is_multiple_of` ベースへ更新。
- P2-2: 着手（第一段）
  - `writer/opcode.rs` に opaque command の主要 code/分類定数を追加。
  - `writer.rs` の `opaque_write_size` / `needs_relocation` / `materialize_opaque` / `resolve_opaque_value` でマジックナンバーを定数参照へ置換。
  - `writer/expr.rs` でも push/expr/direct/disp 系 opcode の分岐を定数参照に統一。
  - `writer.rs` の section code 判定を `SectionKind::from_u8` ベースの helper (`reloc_section_kind`, `is_common_or_xref_section`) に整理し、range/literal 判定を削減。
  - 上記 section 判定 helper を `writer/expr.rs` にも適用し、`0xfc..=0xff` / `0x01..=0x0a` の重複判定を共通化。
  - `Xref` 判定を `is_xref_section` helper へ抽出し、`0xff` 直書きを削減。`writer.rs` の残存 `0x41` 分岐も `OPH_ABS_WORD_ALT` へ統一。
  - `ABS` 判定も `is_abs_section` helper へ抽出し、`0x00` 直書き分岐を `writer`/`expr` で共通化。
  - `writer/expr.rs` の `section_number(current) <= 4` 判定を `is_base_section` helper へ置換し、数値比較ベースの判定を型ベースへ整理。
- P1-3: 着手（第一段）
  - `writer.rs` の `collect_object_relocations` を共通 walker (`walk_opaque_commands`) へ寄せ、section/cursor 走査の重複実装を削減。
  - walker を `walk_commands` に拡張し、`validate_unsupported_expression_commands` の header 事前走査を統合（command 1-pass 化）。
  - 挙動確認: `./tools/run_quality_gate.sh` で `clippy` / `cargo test` / `run_hlkx_regression` 全通。
  - `build_object_initialized_sections` の手書き `obj.commands` 走査を `walk_commands` へ統合し、section/cursor 管理の重複を削減。
  - `compute_g2lk_synthetic_symbols` の opcode 集計も `walk_commands` 経由に統一。
  - `writer/expr.rs` の direct byte/word 系4関数に共通する `xref/common` オペランド判定を `direct_xref_label_no` helper に集約。
  - direct byte/word 系の値検証（定数/xref）を `validate_direct_*` helper に集約し、重複したエラーメッセージ分岐を整理。
  - `writer/expr.rs` の `evaluate_a0` を unary/div-mod/add/sub/binary helper 群へ分割し、`too_many_lines` 許可を廃止。
- `select_archive_members` の `defs/unresolved` 更新ループを helper (`add_defined_symbols`, `extend_unresolved`) 化し、同一処理の重複を削減。
- P1-2: 着手（第一段）
  - `linker.rs` の `resolve_map_output` を `Option<&String>` から `Option<&str>` へ移行し、呼び出し側を `as_deref()` ベースに更新。
  - 文字列所有を要求しない API 形に寄せ、`Path/PathBuf` 中心化の前段整理を実施。
  - `load_indirect_inputs` を `&Path` 引数へ変更し、エラー文言も `path.display()` で統一。
  - `resolve_lib_inputs` の戻り値を `Vec<PathBuf>` に変更し、`expand_inputs` 側で最終段だけ文字列化する構成へ整理。
  - `load_objects_with_requests` 実装本体を `load_objects_with_requests_paths(&[PathBuf], ...)` として分離し、内部処理を `PathBuf` ベースへ移行。
  - `PreparedLink.expanded_inputs` / `expand_inputs` を `Vec<PathBuf>` 化し、`resolve_output_path` / `resolve_map_output` も `PathBuf` 入力へ統一。
  - `Args` のパス系引数を `PathBuf` 中心へ移行（`output`, `inputs`, `indirect_files`, `lib_paths`）。
  - `linker` 側の入力展開・ライブラリ探索・出力パス決定で文字列経由の `PathBuf::from` を削減。
  - 互換性都合で `-p/--map` の「引数省略許容」は維持し、`map` のみ `Option<String>` のまま運用。
- P1-1: 着手（第一段）
  - archive member 選択結果の適用を `HashSet<usize>` から `Vec<bool>` マスクへ変更し、中間集合構築と `contains` 探索コストを削減。
  - `linker` テスト経路の入力ロードも `PathBuf` ベース (`load_objects_with_requests_paths`) に統一し、`String -> PathBuf` 再変換を削減。
  - `select_archive_members` 内の選択済み管理を `HashSet<usize>` から `Vec<bool>` に変更し、ループ内 membership 判定を O(1) 配列参照へ単純化。
  - 入力キュー初期化を `pending.extend(initial_inputs.iter().cloned())` に整理し、反復 push の定型コードを削減。
  - `resolve_output_path` で不要な `PathBuf` 一時変数を削除し、`inputs.first()` 参照から直接 stem/parent を導出する形へ簡素化。
  - `linker` テストの単一入力ケースを `Vec` 生成から `std::slice::from_ref` へ置換し、不要 clone/一時確保を削減。
  - `resolve_lib_inputs` の検索パス初期化を `args.lib_paths.clone()` ベースへ簡素化し、push ループを削減。
  - `run_writes_map_*` テストで `input.clone()` を排除し、`PathBuf` を move して後始末は `dir.join(...)` 参照へ統一。
  - `emit_outputs` の出力パス文字列変換を `to_string_lossy().to_string()` から `Cow<str>` ベースの `as_ref()` 引き渡しに整理し、不要な `String` 確保を削減。
  - `resolve_requested_path` の候補生成ロジックを closure 依存から直列処理へ整理し、探索順を維持したまま読みやすさを改善。
  - `writer/map.rs` でシンボル定義者マップを `String` 保持から owner index 保持へ変更し、xref 表示時のみ名前解決する形にして不要 clone を削減。
  - `writer/map.rs` で object placement 参照を `HashMap` clone から borrow 参照に変更し、セクション先頭位置計算の一時確保を削減。
  - `writer/map.rs` の `build_map_text` 引数を `MapSizes` struct 化し、サイズ引数の乱立と `too_many_arguments` 許可を廃止。

検証:
- `cargo test -q --manifest-path Cargo.toml`: pass
- `./tools/run_hlkx_regression.sh`: All regression cases matched
- `cargo clippy --all-targets --all-features -- -W clippy::all -W clippy::pedantic`: pass

## 残存 `allow(clippy::...)`（2026-02-28 時点）
- `src/cli.rs` の `struct_excessive_bools`:
  - CLI 互換オプション仕様（HLK 互換）をそのまま `clap` で受ける都合で、bool フラグが多いのは設計上許容。
- `src/writer.rs` の `similar_names`（SCD 解析/検証系）:
  - `linfo/sinfo/einfo/ninfo` は元仕様用語に直結しており、安易なリネームよりドメイン語彙維持を優先。
- `src/writer/tests.rs` の `too_many_lines`:
  - 仕様差分を追跡しやすくするため、大規模統合テストはケース密集を維持。
