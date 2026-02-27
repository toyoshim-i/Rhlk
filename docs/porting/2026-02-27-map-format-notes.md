# HLK `-p` / `make_map` 調査メモ (2026-02-27)

## 対象
- `external/hlkx/src/main.s`
  - `ana_opt_b350` (`-p[file]`)
  - `make_map_name`
- `external/hlkx/src/make_map.s`
  - `make_map`

## `-p` オプションの解釈
- `-p` 指定で map 作成フラグを立てる (`MK_MAP_FLAG`)。
- `-p` 直後の文字列を `MAP_NAME` に格納。
- コメント上の仕様:
  - `hlk foo.o -p` -> `foo.map`
  - `hlk -p foo.o` -> `foo.map`
  - `hlk -pbar foo.o` -> `bar.map`
  - `hlk -pfoo` -> error

## map ファイル名決定 (`make_map_name`)
- `-p` 引数が空の場合:
  - `EXEC_NAME` を基準に `.map` へ変換する。
- `-p` 引数がある場合:
  - その名前を使う。
  - 拡張子なしなら `.map` を補完する。
- `DOS _NAMECK` で drive/name/ext に分解し、正規化したうえで再構築。

## map 本文の大まかな生成順 (`make_map`)
- 先頭に罫線 + 出力名。
- `EXEC_ADDRESS` を出力。
- section 情報を順に出力:
  - text, data, bss, common, stack
  - rdata, rbss, rcommon, rstack
- その後、ラベル情報を巡回して出力（`make_map_l` ループ）。

## 現在の `Rhlk` 実装との差分
- 実装済み:
  - `-p[FILE]` 受理
  - map 名導出（`-p` 空時の既定名、`-pfoo` の `.map` 補完）
  - 最小 map 本文（symbol / section / address）
- 未実装:
  - HLK 同等のヘッダ・罫線・section サイズ表レイアウト
  - ラベル一覧の順序/書式の一致
  - SJIS/表示文言の一致

## 次の実装候補
1. section サイズ表を HLK 順（text/data/bss/common/stack/r*）で出力。
2. symbol 行の並び順を HLK と照合して合わせる（name/addr/section の優先順確認）。
3. map 比較を回帰ハーネスに追加（まずは normalize 後のテキスト比較）。
