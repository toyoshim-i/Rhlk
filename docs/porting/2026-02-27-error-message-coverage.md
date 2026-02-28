# HLK エラーメッセージカバレッジ監査 (2026-02-27)

> Status: Historical snapshot。  
> 本文のカバレッジ数値は当時の集計値。最新の最終評価は
> [2026-02-27-final-executable-compare.md] を参照。

対象:
- `external/hlkx/src/make_exe.s` の主要エラーメッセージ
- `tools/run_hlkx_regression.sh` 実行結果 (`artifacts/hlkx-regression/orig/*.msg.norm`)

監査コマンド:

```bash
./tools/run_hlkx_regression.sh
./tools/audit_hlkx_message_coverage.sh
```

## 結果
- 総数: 16 パターン
- カバー済み: 13
- 未カバー: 3

## カバー済みパターン
- `アドレス属性シンボルの値をバイトサイズで出力 in ` (`adrs_not_long`)
- `アドレス属性シンボルの値をワードサイズで出力 in ` (`adrs_not_long`)
- `ゼロ除算 in ` (`div_zero`)
- `不正な式 in ` (`exp`)
- `バイトサイズ(-$80〜$ff)で表現できない値 in ` (`overflow`)
- `バイトサイズ(-$80〜$7f)で表現できない値 in ` (`overflow`)
- `ワードサイズ(-$8000〜$ffff)で表現できない値 in ` (`overflow`)
- `ワードサイズ(-$8000〜$7fff)で表現できない値 in ` (`overflow`)
- `複数の実行開始アドレスを指定することはできません in ` (`dup_entry`)
- `再配置テーブルが使われています: ` (`r_reltbl`)
- `再配置対象が奇数アドレスにあります: ` (`reltbl_odd`)
- `実行開始アドレスがファイル先頭ではありません: ` (`r_entry`)
- `MACS形式ファイルではありません: ` (`makemcs_not_mcs`)

## 未カバーパターン
- `32ビットディスプレースメントにアドレス属性シンボルの値を出力 in `
- `計算用スタックが溢れました in `
- `計算用スタックに値がありません in `

## コメント
- 既存の `.s` テストだけでは、上記3つの発火入力を安定生成できていない。
- 追加方針:
  1. まず `run68 + has060` で再現可能な最小 `.s` を探索（優先）
  2. 難しい場合は、最小 `.o` フィクスチャ（手生成）を `tests/compat/fixtures` として導入
  3. 回帰ハーネスで「ソースから生成する `.o`」と「固定 `.o`」を混在実行できるよう拡張

[2026-02-27-final-executable-compare.md]: 2026-02-27-final-executable-compare.md
