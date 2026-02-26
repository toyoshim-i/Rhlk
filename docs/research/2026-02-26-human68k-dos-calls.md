# Human68k DOS コール調査 (HLKX 実使用分)

## 対象
`external/hlkx/src/*.s` で実際に呼ばれている DOS コール:

`_CLOSE, _CREATE, _EXIT2, _FILES, _FPUTS, _GETENV, _MALLOC, _MALLOC3, _NAMECK, _OPEN, _PRINT, _PUTCHAR, _READ, _SEEK, _SETBLOCK, _WRITE`

## コール番号
参照: Data Crystal `X68k/DOSCALL`。

| call | code | 用途(移植観点) |
| --- | --- | --- |
| `_PUTCHAR` | `$ff02` | 1文字出力 |
| `_PRINT` | `$ff09` | 文字列出力 |
| `_FPUTS` | `$ff1e` | file handle へ文字列出力 |
| `_NAMECK` | `$ff37` | パス展開(91-byte buffer) |
| `_CREATE` | `$ff3c` | 出力ファイル作成 |
| `_OPEN` | `$ff3d` | 入力/ライブラリ読み込み |
| `_CLOSE` | `$ff3e` | ファイルクローズ |
| `_READ` | `$ff3f` | ファイル読み込み |
| `_WRITE` | `$ff40` | ファイル書き込み |
| `_SEEK` | `$ff42` | ファイルポインタ移動 |
| `_MALLOC` | `$ff48` | メモリ確保 |
| `_SETBLOCK` | `$ff4a` | メモリブロック調整 |
| `_EXIT2` | `$ff4c` | 終了コード付き終了 |
| `_FILES` | `$ff4e` | ワイルドカード検索 |
| `_GETENV` | `$ff83` | 環境変数参照 |
| `_MALLOC3` | `$ff90` | 060turbo.sys 拡張 (HLKX 定義より) |

## Rust 移植での扱い

- 直接 DOS コールは使わず、`std` ベースでホスト OS API に置換する。
- ワイルドカード展開はシェルに委譲する（`_FILES` 互換実装はしない）。
- 8.3 名称規則や X68k 固有のパス正規化はエミュレートせず、ホスト OS の挙動に従う（`_NAMECK` 互換実装はしない）。
- `_MALLOC/_SETBLOCK/_MALLOC3` は Rust 側のメモリ管理に置換し、確保失敗時のエラー処理のみ互換化する。

## 参照
- https://datacrystal.tcrf.net/wiki/X68k/DOSCALL
