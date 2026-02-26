# run68 ベース回帰ハーネス

## 目的
オリジナル HLK と rhlk の挙動差分を、既存 `external/hlkx/tests` を使って継続検出する。

比較対象:
- 標準出力 (`stdout`)
- 標準エラー (`stderr`)
- 終了コード
- 生成物バイナリ (`.x` / `.r`)

## 追加ファイル
- `tools/run_hlkx_regression.sh`
- `tests/compat/hlkx_cases.tsv`

## 前提
- `run68` が利用可能であること
- `has060.x`（アセンブラ）を `run68` 経由で実行可能であること
- オリジナル HLK（例: `external/hlkx/build/hlk.x`）を `run68` 経由で実行可能であること

## 実行方法
デフォルト設定のまま:

```bash
./tools/run_hlkx_regression.sh
```

環境に応じてコマンドを上書き:

```bash
HAS_CMD="run68 has060.x" \
HLK_CMD="run68 /path/to/hlk.x" \
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

## run68 の取り込み
`run68` 自体をリポジトリ参照したい場合は、必要に応じて `external/` 配下へ submodule 追加する。
ただし実行環境依存があるため、初期段階では「ローカル導入済み run68 を利用」する運用を優先する。
