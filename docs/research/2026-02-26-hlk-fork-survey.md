# HLK フォーク調査メモ (2026-02-26)

## 目的
X68000 向けリンカ HLK の「現行で実作業が進んでいるフォーク」を特定し、Rust 移植元として固定する。

## 調査方法
GitHub API で `HLK` + `X68000` を検索し、`updated_at` / `pushed_at` / `archived` を比較した。

- API: `https://api.github.com/search/repositories?q=HLK+X68000&sort=updated&order=desc&per_page=20`
- API: `https://api.github.com/search/repositories?q=%22SILK+Hi-Speed+Linker%22&sort=updated&order=desc&per_page=20`
- API: `https://api.github.com/search/repositories?q=topic:hlk+topic:x68000&sort=updated&order=desc&per_page=50`

## 結果

| repo | 説明 | updated_at | pushed_at | archived |
| --- | --- | --- | --- | --- |
| `kg68k/hlkx` | HLK evolution 後継 | 2025-07-26 | 2025-07-26 | false |
| `kg68k/hlk-ev` | HLK evolution | 2025-02-04 | 2024-03-10 | true |

`pushed_at` が新しく、かつ非 archived のため、移植元は `kg68k/hlkx` を採用する。

## 取得済みソース

- `external/hlkx` (origin: `https://github.com/kg68k/hlkx.git`)
  - commit: `29ca73cfbfe62c33292effaca14762d7ccac853a`
- `external/hlk-ev` (比較用, origin: `https://github.com/kg68k/hlk-ev.git`)
  - commit: `6ebf5029c435fc3ac62d293467e5fd34588048d8`

## 補足
`hlkx` は Assembly 実装で、`README.md` に「SILK Hi-Speed Linker v3.01 の改造版・後継」と明記されている。
