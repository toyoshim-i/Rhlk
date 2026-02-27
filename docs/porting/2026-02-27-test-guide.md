# テスト実行ガイド (2026-02-27)

## 目的
Rhlk のテストを、ローカル環境で再現可能な形で実行するための手順をまとめる。

## テストの種類
- Rust unit/integration test
  - 対象: `Rhlk/src/*` の unit test、`Rhlk/tests/*` の integration test
  - 実行: `cargo test`
- HLK 互換回帰テスト（run68 ベース）
  - 対象: `external/hlkx/tests` を入力に、原版 HLK と rhlk の出力比較
  - 比較: messages（stdout+stderr 結合後）、終了コード、生成物 (`.x/.r/.mcs/.map`)
  - 実行: `./tools/run_hlkx_regression.sh`
- エラーメッセージ網羅監査
  - 対象: 主要エラーメッセージ 16 パターンのカバー状況
  - 実行: `./tools/audit_hlkx_message_coverage.sh`

## 必要な実行環境
- Rust ツールチェーン (`cargo`, `rustc`)
- 共通コマンド
  - `bash`, `git`, `perl`, `diff`, `cmp`, `iconv`
  - `curl`, `unzip`, `rg`（`setup_human68k_binaries.sh` 利用時）
- run68 実行環境（どちらか）
  - `external/run68x/build/run68` をビルドして使う
  - もしくは `PATH` 上の `run68` を使う

## 初期セットアップ
1. submodule を取得
```bash
git submodule update --init --recursive
```

2. run68x をビルド（推奨）
```bash
cmake -S external/run68x -B external/run68x/build
cmake --build external/run68x/build
```

3. Human68k バイナリを取得（推奨）
```bash
./tools/setup_human68k_binaries.sh
```

このスクリプトは以下を配置する。
- `external/toolchain/bin/hlkx.r`
- `external/toolchain/bin/has060x.x`
- `external/toolchain/bin/u8tosj.r`

## テスト実行手順（推奨順）
1. Rust テスト
```bash
cargo test -q --manifest-path Rhlk/Cargo.toml
```

2. HLK 互換回帰
```bash
./tools/run_hlkx_regression.sh
```

3. メッセージカバレッジ監査（任意）
```bash
./tools/audit_hlkx_message_coverage.sh
```

## 回帰ハーネスの出力
- 出力先: `artifacts/hlkx-regression/`
- 内訳:
  - `orig/`: 原版 HLK の実行結果
  - `rhlk/`: rhlk の実行結果
  - `diff/`: 差分ログ（失敗ケースのみ）

## 環境差分がある場合の上書き
`run_hlkx_regression.sh` は以下環境変数でコマンドを上書き可能。

```bash
HAS_CMD="/path/to/run68 has060.x" \
HLK_CMD="/path/to/run68 /path/to/hlk.x" \
RHLK_CMD="cargo run --manifest-path /abs/path/Rhlk/Cargo.toml --quiet --" \
./tools/run_hlkx_regression.sh
```

## よくある失敗
- `missing linker binary`:
  - `tools/setup_human68k_binaries.sh` を実行するか、`external/hlkx/build/hlk.x` を用意する。
- `missing assembler command`:
  - `has060x.x` を取得するか、`has060.x` を `PATH` に配置する。
- `missing source and fixture for *.o`:
  - `external/hlkx/tests` の対応 `.s` か `tests/compat/fixtures/*.o` が不足している。
