# Rust 移植計画 (初版)

## ゴール
HLKX 相当機能を、ホスト OS ネイティブ実行可能な Rust 実装として段階移植する。

## ステップ

1. `Phase 0`: 解析固定
- `hlkx` のオプションと既知コマンドを仕様化
- 既存 `tests/` を収集し Golden 化

2. `Phase 1`: 読み取り系
- object/archive/lib parser
- command stream を IR 化

3. `Phase 2`: 解決系
- xdef/xref/common 解決
- section 配置と align

4. `Phase 3`: 出力系
- `.x` 生成
- `.r` / `--makemcs` / `--omit-bss` 対応

5. `Phase 4`: 互換検証
- HLKX とのバイナリ比較
- 差分許容条件を定義

## モジュール対応 (asm -> Rust)
- `main.s` -> `cli`, `config`, `driver`
- `object.s` -> `format::obj`, `parser`, `resolver`
- `label.s` -> `symbol`
- `make_exe.s` -> `writer::xfile`
- `make_map.s` -> `writer::map`
- `file.s` -> `hostfs`

## 互換性ポリシー
- CLI 互換は高優先。
- DOS 呼び出しの「番号互換」ではなく「挙動互換」を目標。
- ワイルドカードはシェル展開を前提とし、`_FILES` 相当の独自展開は実装しない。
- 8.3 規則や X68k 固有のパス解釈は再現せず、ファイル名解決はホスト OS に委譲する。
- 未対応機能は明示エラーにする (黙って無視しない)。

## 直近タスク
- parser 最小実装: `c0/d0/e0/00/10/20/30/b2` を先行対応
- オブジェクト 1 個入力で `.x` へ変換する最小パスを作る

## 進捗 (2026-02-27)
- `Rhlk/src/format/obj.rs` に最小 parser を実装し、`c0/d0/e0/00/10/20/30/b2` を読み取り可能にした。
- `Rhlk/src/linker.rs` から入力ファイルを読み込み、parser を実行する導線を追加した。
- `Rhlk/src/resolver.rs` を追加し、section 使用量・シンボル・xref・request・start address を集約する resolver を実装した。
- `Rhlk/src/layout.rs` を追加し、オブジェクトごとの align を考慮した section 配置計算（text/data/bss/stack/r*）を実装した。
- `Rhlk/src/layout.rs` に common/rcommon/rlcommon の統合ルールを追加し、同名 common の最大サイズ採用、通常定義での上書き、種別衝突の診断カウントを実装した。
- `Rhlk/src/writer.rs` を追加し、現在対応済みコマンドから `.r` と最小 `.x`（HUヘッダ + text/data + bss size）を生成して `-o` で書き出す writer を実装した。
- `.x` writer を拡張し、定義済みシンボルから symbol table を生成してヘッダの `symbol_data size` に反映した（reloc/SCD は未対応）。
- parser 対応コマンドを拡張し、`4x/5x/6x/8x/9x/a0/b0ff/e00c/e00d` 系を `Opaque` として読み取り可能にした（意味解釈は未実装）。
- `.x` writer を拡張し、`42/46/52/56` の long 参照から最小 relocation table を生成してヘッダの `relocate_size` に反映した。
- relocation 抽出対象を `6a` まで拡張し、long 差分参照命令を relocation table に反映した。
- relocation 抽出対象を `9a00` まで拡張し、式評価結果の long 書き込みも relocation table に反映した。
- `ObjectFile` に SCD tail を保持し、`.x` writer で line/info/name を raw pass-through 連結してヘッダの `SCD size` 3項目に反映した。
- `.r` writer を拡張し、`--omit-bss` 未指定時は BSS+COMMON+STACK 分のゼロ領域を追記するようにした。
- `--rn` オプションを追加し、通常 `-r` では `reltbl 非空` と `exec!=先頭` をエラーにし、`--rn` 指定時のみ強制作成するチェックを実装した。
- `--makemcs` を最小対応し、`-r` 強制に加えて `MACS/DATA` シグネチャ検査とヘッダ内ファイルサイズ埋め込み(offset 10)を実装した。
- SCD pass-through を拡張し、line table の `location!=0` は `text_pos` 加算、`location==0` は `sinfo_pos` 加算で再配置する最小補正を実装した。
- SCD info pass-through を拡張し、先頭 `sinfo_count*18` バイト（sinfo entry）の `val.l` を section 別配置オフセットで再配置する最小補正を実装した。
- SCD info pass-through をさらに拡張し、einfo entry の `d6==0` ケースに対して `sinfo` 参照インデックスへ累積 `sinfo_pos` を加算する最小補正を実装した。
- SCD info pass-through をさらに拡張し、einfo entry の `d6!=0` ケースに対して section 別配置オフセットを `off.l` へ加算する最小補正を実装した。
- `make_scdinfo` の `make_scd_b550..b556` 相当（`text/data/bss/rdata/rbss/rldata/rlbss`）をテストで網羅し、残分岐の先行消化を完了した。
