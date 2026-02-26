# 最終実行ファイル比較 (2026-02-27)

## 実施内容
1. `has060xx` を Shift_JIS 変換後にビルド
2. `hlkx` を Shift_JIS 変換後にビルド (`external/hlkx/build/hlkx.x`)
3. 同一オブジェクト入力をオリジナル HLK と `rhlk` でリンクし、生成 `.x` を比較

## ビルド結果
- `external/has060xx/build/has060x.x` 生成成功
- `external/hlkx/build/hlkx.x` 生成成功

## 初回比較結果
### Case A: `xdef.o` 単体リンク
- 入力: `xdef.o`
- 出力: `orig_xdef.x` vs `rhlk_xdef.x`
- 結果: **一致** (`sha256` 同一)

### Case B: 外部シンボル呼び出しを含む最小2オブジェクト
- 入力: `sample_main.o + sample_sub.o` (`sample_main.s` は `jsr func`)
- 出力: `orig.x` vs `rhlk.x`
- 結果: **不一致**
  - `orig.x` 100 bytes
  - `rhlk.x` 98 bytes
  - 先頭差分はコード/再配置付近（`text` 領域）に発生

## 原因と修正
- 原因1: `Opaque` コマンドが初期化セクションへ実バイト出力されていなかった
  - `RawData` のみ連結していたため、`42ff` 等の値埋め込み領域がゼロのまま
- 原因2: `Opaque` 分のサイズをセクションバッファに反映しておらず、後続 `RawData` が前詰めになっていた
- 原因3: `text/data` バッファの最終取り出しが `Opaque` パッチ適用前だった

修正:
- `Opaque` の書き込みサイズをセクション構築時に反映
- `42ff/52ff` など主要な `Opaque` 値を解決して実バイトへ反映
- `lo=0xff` (xref) の再配置判定をシンボル解決ベースへ拡張
- `Opaque` パッチ適用後の `text/data` を最終イメージへ使用

## 修正後比較結果
- Case A (`xdef.o`): 一致
- Case B (`sample_main.o + sample_sub.o`): **一致**
  - `orig.x` と `rhlk.x` の `sha256` 同一

## まとめ
- 再配置なし/あり双方の比較ケースで最終 `.x` 一致を確認。
- 既存回帰ハーネス (`9/9`) も継続して一致。
