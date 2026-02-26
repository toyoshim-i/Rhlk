# 回帰ハーネス初回実行結果 (2026-02-27)

実行コマンド:

```bash
./tools/setup_human68k_binaries.sh
./tools/run_hlkx_regression.sh
```

## 結果サマリ
- 対象 8 ケース
- 一致 0 / 不一致 8

## ケース別の主な差分
1. `adrs_not_long/div_zero/dup_entry/exp/overflow/reltbl_odd`
- オリジナル HLK: エラー検出で `rc=1`
- rhlk: 成功扱いで `rc=0` になっている
- 生成物 `.x` の有無も不一致

2. `r_entry/r_reltbl`
- `rc` は一致（どちらも `1`）
- ただしエラーメッセージの出力先が異なる:
  - オリジナル HLK: `stdout`
  - rhlk: `stderr`

## 現時点の評価
- ハーネス自体は `stdout/stderr/rc/生成物` の比較として機能。
- 現在の最大ギャップは「式評価/範囲チェック系エラーが rhlk で未実装」のため、6ケースが成功してしまう点。
- メッセージ互換は、まず `rc` と生成物互換を優先し、その後に出力先/文言差分を詰めるのが効率的。

## 次の実装優先
1. `exp/div_zero/overflow/adrs_not_long` に対応する式評価エラー検出の導入（`rc=1` 化）
2. `dup_entry` の重複開始アドレス検出
3. `reltbl_odd` の再配置テーブル妥当性検証
4. メッセージ互換方針（stdout/stderr と文言）の固定
