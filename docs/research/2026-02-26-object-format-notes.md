# HLK オブジェクト/実行形式メモ

## 一次資料
- `external/hlkx/kaiseki.txt`
- `external/hlkx/scdkaiseki.txt`

## オブジェクトコマンドの骨子

- `00 00`: end
- `10`: 生データ
- `20`: section 切り替え
- `30`: 領域確保 (`ds.b` 相当)
- `4x/5x/6x`: relocation / label 演算
- `80/90/a0`: 演算スタック命令
- `b0/b2`: シンボル定義 (`xdef/xref/common`)
- `c0`: section header
- `d0 00`: file 名
- `e0 00`: start address
- `e0 01`: request

HLKX 拡張:
- `.ctor/.dtor`: `4c 01`, `4d 01`
- ヘッダ: `c0 0c` (`ctor`), `c0 0d` (`dtor`)
- フラグ: `e0 0c` (`.doctor`), `e0 0d` (`.dodtor`)

## 実行形式(.x)ヘッダ (64 bytes)
`external/hlkx/src/hlk.mac` の `X_*` 定義が実装上の正本。

- magic: `'HU'`
- load mode
- base/exec address
- text/data/bss/reloc/symbol size
- SCD line/symbol/name table size
- bind offset

## アーカイブ
`kaiseki.txt` に `.a` / `.l` の概要あり。Rust 移植は読み取り専用 parser を先に作る。

## Rust 移植で最初に固定すべき仕様
1. object command の strict parser (未知コマンドはエラー)
2. section size 計算と align ルール
3. relocation table 生成 (`.x` と `.r` の差分)
4. label/xref/common 解決順序
5. SCD 情報は初版では pass-through (破壊しない) を優先

## SCD einfo (`d6!=0`) の分岐メモ
`external/hlkx/src/make_exe.s` の `make_scd_b550..b556` より、`off.l` 補正の実装先は次の 7 分岐。

- `text(1)`: `+ obj_list_text_pos`
- `data(2)`: `+ obj_list_data_pos - TEXT_SIZE`
- `bss(3)`: `+ obj_list_bss_pos - OBJ_SIZE`
- `rdata(5)`: `+ obj_list_rdata_pos`
- `rbss(6)`: `+ obj_list_rbss_pos`（旧補正コードはコメントアウト）
- `rldata(8)`: `+ obj_list_rldata_pos`（旧補正コードはコメントアウト）
- `rlbss(9)`: `+ obj_list_rlbss_pos`（旧補正コードはコメントアウト）

`stack(4)/rstack(7)/rlstack(10)` はこの分岐表では扱われず、該当時は `make_scd_err2` へ落ちる。
Rust 側でも互換方針としてこの 3 種は pass-through せず、`.x` 生成を明示エラーで停止する。
`common/rcommon/rlcommon` 参照 (`00fc..00fe/fffc..fffe`) は最小対応として、entry 名（8-byte name または ninfo 参照）から xdef を解決し、`sect=0003/0006/0009` に正規化して再配置する。
未解決名や不整合 section は明示エラー扱いとする。
