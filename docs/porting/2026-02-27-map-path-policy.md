# map ヘッダのパス扱いポリシー (2026-02-27)

## 方針
- map ヘッダ2行目（実行ファイルパス）は、`rhlk` 実行時の実パスをそのまま出力する。
- 互換回帰比較では、この1行のみ `<PATH>` に正規化して比較する。
- それ以外の map 本文（区切り線、`exec`、section 行、`xref/xdef` ブロック）は raw 比較で完全一致を要求する。

## 理由
- 回帰ハーネスでは `orig` と `rhlk` で成果物出力ディレクトリが意図的に分かれており、ヘッダ2行目だけは常にパス差分が発生する。
- この差分はリンクロジック差分ではなく実行環境差分なので、比較対象から除外する。

## 実装位置
- map 出力: `src/writer.rs` (`build_map_text`)
- raw 比較正規化: `tools/run_hlkx_regression.sh` (`normalize_map_raw`)
- 回帰方針の説明: `docs/porting/2026-02-27-regression-harness.md`
