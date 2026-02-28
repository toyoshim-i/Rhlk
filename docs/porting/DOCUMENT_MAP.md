# Porting Document Map

`docs/porting` 配下の文書を、用途ごとに分類した台帳。

## Authoritative（常に最新へ更新）
- [2026-02-27-final-executable-compare.md]
  - 最終比較結果と結論。
- [2026-02-27-regression-harness.md]
  - 回帰ハーネスの前提・実行方法・比較ルール。
- [2026-02-27-map-path-policy.md]
  - map ヘッダパス差分の扱い方針。
- [2026-02-27-test-guide.md]
  - 開発者向けテスト実行ガイド。
- [2026-02-27-rust-idiomatic-refactor-priority.md]
  - リファクタ優先度、進捗、残存ルール。

## Reference（参照用。必要時のみ更新）
- [2026-02-27-command-coverage.md]
  - 命令カバレッジ監査。
- [2026-02-27-phase1-command-audit.md]
  - フェーズ1の実装監査ログ。
- [2026-02-27-gap-audit.md]
  - 主要ギャップ監査。

## Historical（履歴スナップショット。原則不変）
- [2026-02-26-rust-port-plan.md]
- [2026-02-27-regression-first-run.md]
- [2026-02-27-error-message-coverage.md]
- [2026-02-27-map-format-notes.md]
- [2026-02-27-remaining-roadmap.md]

## 更新ルール
- 新しい最終判断は `Authoritative` に反映する。
- 途中経過は `Reference` か新規文書に記録する。
- `Historical` は書き換えず、追記が必要なら別ファイルを追加する。

[2026-02-27-final-executable-compare.md]: 2026-02-27-final-executable-compare.md
[2026-02-27-regression-harness.md]: 2026-02-27-regression-harness.md
[2026-02-27-map-path-policy.md]: 2026-02-27-map-path-policy.md
[2026-02-27-test-guide.md]: 2026-02-27-test-guide.md
[2026-02-27-rust-idiomatic-refactor-priority.md]: 2026-02-27-rust-idiomatic-refactor-priority.md
[2026-02-27-command-coverage.md]: 2026-02-27-command-coverage.md
[2026-02-27-phase1-command-audit.md]: 2026-02-27-phase1-command-audit.md
[2026-02-27-gap-audit.md]: 2026-02-27-gap-audit.md
[2026-02-26-rust-port-plan.md]: 2026-02-26-rust-port-plan.md
[2026-02-27-regression-first-run.md]: 2026-02-27-regression-first-run.md
[2026-02-27-error-message-coverage.md]: 2026-02-27-error-message-coverage.md
[2026-02-27-map-format-notes.md]: 2026-02-27-map-format-notes.md
[2026-02-27-remaining-roadmap.md]: 2026-02-27-remaining-roadmap.md
