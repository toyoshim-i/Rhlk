# run68 ベース回帰ハーネス

## 目的
オリジナル HLK と rhlk の挙動差分を、既存 `external/hlkx/tests` を使って継続検出する。

比較対象:
- メッセージ出力（`stdout` + `stderr` を結合したストリーム）
- 終了コード
- 生成物バイナリ (`.x` / `.r` / `.mcs`)
- map 生成時の `.map` バイナリ

補足:
- 出力チャネルの違い（`stdout` vs `stderr`）は無視し、結合後の文字列を比較する。
- オリジナル HLK 側メッセージは `Shift_JIS` を `UTF-8` に正規化して比較する。
- パスを含む一部エラーメッセージは、比較時にパス部分を `<PATH>` に正規化する。
- 生成物比較は `output_ext` 列を正とし、原版 HLK が `rc=0` のケースでは期待生成物の存在を必須とする。
- 両実装で期待生成物が存在する場合は、`cmp -s` でバイト一致を必須とする。
- `.map` は「実フォーマット比較（raw）」を実施し、ヘッダ2行目の実行ファイルパスだけ正規化して比較する。
- 互換性確認の補助として `name/addr/section` 抽出の正規化比較も併用する。
- `.map` 比較は両実装の終了コードが `0` のケースに限定する。
- map ヘッダのパス扱い方針は [`2026-02-27-map-path-policy.md`](./2026-02-27-map-path-policy.md) を正とする。

## 追加ファイル
- `tools/run_hlkx_regression.sh`
- `tools/run_quality_gate.sh`
- `tools/lib/regression_normalize.sh`
- `tools/lib/regression_case.sh`
- `tests/compat/hlkx_cases.tsv`

## 前提
- `run68` 互換実行環境があること（`external/run68x/build/run68` を優先利用）
- `has060.x`（アセンブラ）を `run68` 経由で実行可能であること
- オリジナル HLK（例: `external/hlkx/build/hlk.x`）を `run68` 経由で実行可能であること

## run68x のビルド
`run68x` は submodule (`external/run68x`) として取り込まれている。

```bash
cmake -S external/run68x -B external/run68x/build
cmake --build external/run68x/build
```

`external/run68x/build/run68` が存在する場合、回帰スクリプトはこれを自動で使う。
存在しない場合は `PATH` 上の `run68` を使う。

## Human68k バイナリの取得（推奨）
ソースビルド前に、配布済みバイナリを取得して回帰を回せる。

```bash
./tools/setup_human68k_binaries.sh
```

取得先:
- `external/toolchain/bin/hlkx.r`
- `external/toolchain/bin/has060x.x`
- `external/toolchain/bin/u8tosj.r`

回帰スクリプトは上記が存在する場合に自動で優先利用する。

## HLKX ビルド前提
`external/hlkx/build/hlk.x` が必要。これは Human68k ツールチェーンで生成する。

関連 submodule:
- `external/has060xx`（assembler 系）
- `external/u8tosj`（UTF-8 -> Shift_JIS 変換）

実環境では、`has060.x` と `u8tosj` を実行可能な形で用意し、`external/hlkx` をビルドする。
`hlkx` 側の変換/ビルドは `external/hlkx/README.md` の手順に従う。

`has060.x` がアーカイブ配布の場合は、別途 `lhasa` などで展開して配置する。

## 実行方法
品質ゲート（`clippy` + `cargo test` + 回帰）をまとめて回す:

```bash
./tools/run_quality_gate.sh
```

デフォルト設定のまま:

```bash
./tools/run_hlkx_regression.sh
```

環境に応じてコマンドを上書き:

```bash
HAS_CMD="/path/to/run68 has060.x" \
HLK_CMD="/path/to/run68 /path/to/hlk.x" \
RHLK_CMD="cargo run --manifest-path /abs/path/Rhlk/Cargo.toml --quiet --" \
./tools/run_hlkx_regression.sh
```

## 出力
成果物と差分は `artifacts/hlkx-regression/` 以下に出力される。

- `orig/`: オリジナル HLK 実行結果
- `rhlk/`: rhlk 実行結果
- `diff/`: ケースごとの差分ログ（失敗時のみ）

## ケース追加
`tests/compat/hlkx_cases.tsv` に `TSV` で追記する。

列:
1. `case_name`
2. `flags` (`-r` など)
3. `objects`（空白区切り）
4. `output_ext`（`x` または `r`）

## 現在の一致状況 (2026-02-27)
- map 比較ケースを含めて全ケース一致（`map_xdef`, `map_d32_adrs` を含む）
- メッセージ比較は `stdout+stderr` 結合後に文字コード・パス正規化を適用して実施
- 基本オプション互換ケースも一致:
  - `-x`, `-b`, `-g`, `-0`, `-an`, `-w`

## 品質ゲート更新 (2026-02-28)
- `tools/lib/regression_case.sh` の生成物チェックを厳格化。
- 目的は「リンク成功」だけでなく「原版 HLK 生成物をゴールデンとした一致」を品質ゲートで継続保証すること。

## run68 の取り込み
`run68` 自体をリポジトリ参照したい場合は、必要に応じて `external/` 配下へ submodule 追加する。
ただし実行環境依存があるため、初期段階では「ローカル導入済み run68 を利用」する運用を優先する。
