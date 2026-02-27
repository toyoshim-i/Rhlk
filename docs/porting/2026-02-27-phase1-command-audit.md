# Phase 1: Command Execution Audit (2026-02-27)

## Scope
`external/hlkx/src/make_exe.s` の命令実行系（`wrt_lbl_*`, `wrt_stk_*`, `a0xx`）と `Rhlk/src/writer.rs` 実装の対応監査。

## Handler Inventory (HLK)
- `wrt_lbl_40xx/41xx/42xx/43xx/45xx/46xx/47xx`
- `wrt_lbl_50xx/51xx/52xx/53xx/55xx/56xx/57xx`
- `wrt_lbl_65xx/69xx/6axx/6bxx`
- `wrt_stk_9000/9100/9200/9300/9600/9900/9a00`

## Rust Side Entry Points
- parser support: `format::obj::is_supported_opaque`, `read_opaque_payload`
- value materialization: `writer::materialize_opaque`, `resolve_opaque_value`
- stack writes: `writer::materialize_stack_write_opaque`
- expression checks/eval: `writer::classify_expression_errors`, `evaluate_a0`

## This Turn Progress
- `65/69/6a` の `lo=02/03/04` ケースを unit test 追加
- `6b` の `lo=02/03/04` ケースを unit test 追加
- 既存 `6b05/06/07/08/09/0a` と合わせて `6b01..0a` がテスト網羅
- `cargo test` で全通（118 passed）

## Added Tests
- `materializes_6502_word_displacement_data`
- `materializes_6503_word_displacement_bss`
- `materializes_6504_word_displacement_stack`
- `materializes_6902_word_displacement_alias_data`
- `materializes_6903_word_displacement_alias_bss`
- `materializes_6904_word_displacement_alias_stack`
- `materializes_6a02_long_displacement_data`
- `materializes_6a03_long_displacement_bss`
- `materializes_6a04_long_displacement_stack`
- `materializes_6b02_byte_displacement_data`
- `materializes_6b03_byte_displacement_bss`
- `materializes_6b04_byte_displacement_stack`

## Remaining Focus (Next Slice)
1. `40/41/42/43/45/46/47` と `50/51/52/53/55/56/57` の `lo=00/01..0a/fc..ff` 分岐を表で可視化
2. それぞれの「値範囲チェック」「section 制約」「relocation 判定」の差分確認
3. 未網羅分岐を unit + compat に追加（テスト先行）
