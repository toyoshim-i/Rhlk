# make_exe 命令カバレッジ監査 (2026-02-27)

> Status: Historical snapshot.  
> 最新の最終結果は [README.md] の「現在の正」セクションを参照。

## 目的
「実装済みだが未テスト」の取りこぼしを避けるため、命令ファミリ単位でテスト有無を管理する。

## 現状サマリ
- `writer.rs` の unit test と run68 回帰で、主要な失敗系は固定済み。
- ただし `make_exe.s` の全コマンドを網羅したとはまだ言えない。

## コマンドファミリ別

### 4x/5x 直接・オフセット書き込み
- 実装: あり（`40/41/42/43/45/46/47/50/51/52/53/55/56/57`）
- unit test:
  - 既存: `42ff`, `4201`
  - 追加: `45ff`, `47ff`, `55ff`, `57ff`
  - 追加: `4605/4606/4607/4609/460a`, `5608`（r系 `lo=05/06/07/08/09/0a`）
- 未カバー:
  - `56{05,06,07,09,0a}` は unit test 追加済み（未カバー解除）

### 65/69/6a/6b 相対ディスプレースメント
- 実装: あり（`65/69/6a/6b`）
- unit test:
  - 追加: `6501`, `6901`, `6a01`, `6b01`
  - 追加: `6b05/06/07/08/09/0a`（r系セクション）

### 80/a0 計算スタック
- 実装: あり（診断中心）
- unit test:
  - 既存: `8001/a001`（未実装式エラー）
  - 追加: `a002`
  - 追加: `a0` 定数二項演算 (`09,0c,0d,0e,11..1d`) と `0a/0b`
- run68 回帰:
  - 追加: `a0_neg/a0_not/a0_high/a0_low/a0_highw/a0_loww`
  - 追加: `a0_attr_neg`（text属性シンボルでのエラー系）
  - 追加: `a0_attr_add/a0_attr_sub`（text属性シンボル + 定数の二項演算）
  - 追加: `a0_attr_subsym/a0_attr_addsym`（text属性シンボル同士の `-` / `+`）
  - 追加: `a0_attr_mulsym/divsym/modsym/andsym/orsym/xorsym/shlsym/shrsym`

### 90/91/92/93/96/99/9a stack write
- 実装: あり（underflow診断は `90/91/92/93/96/99/9a`）
- unit test:
  - 追加: `9200/9600/9a00` underflow
  - 追加: `8000 -> 9000`, `8000 -> 9200` の正常値出力
- run68 回帰:
  - 追加: `stk91_unary`, `stk93_unary`
  - 追加: `stk96_unary`, `stk99_unary`, `stk9a_unary`
  - 追加: `stack_under_9200/9600/9a00`

## 非命令系の残り
- arc/lib/request 解決フロー
- CLI 互換オプション群
- map 出力 (`-p`)
- SCD fixup の完全互換

## 次にやること
1. `9x` 正常系のfixture追加（`96/99/9a` + a0演算経由）
2. 非定数属性を含む `a0` 比較ケース追加
3. arc/lib/request フローの段階移植（テスト先行）

[README.md]: README.md
