#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
OUT_DIR="${ROOT_DIR}/tests/compat/fixtures"
mkdir -p "${OUT_DIR}"

patch_size() {
  local out="$1"
  local sz
  sz=$(stat -c%s "$out")
  printf '%010x' "$sz" | xxd -r -p | dd of="$out" bs=1 seek=1 conv=notrunc status=none
}

write_hex_obj() {
  local out="$1"
  local hex="$2"
  xxd -r -p > "$out" <<HEX
$hex
HEX
  patch_size "$out"
}

ROOT_RUN68_BIN="${ROOT_DIR}/external/run68x/build/run68"
ROOT_HAS_BIN="${ROOT_DIR}/external/toolchain/bin/has060x.x"
if [[ -x "${ROOT_RUN68_BIN}" && -f "${ROOT_HAS_BIN}" ]]; then
  HAS_ARR=("${ROOT_RUN68_BIN}" "${ROOT_HAS_BIN}")
elif command -v has060.x >/dev/null 2>&1; then
  HAS_ARR=("has060.x")
else
  HAS_ARR=()
fi

# d32_adrs_main.o
# Hand-crafted valid object stream to trigger:
# "32ビットディスプレースメントにアドレス属性シンボルの値を出力"
write_hex_obj "${OUT_DIR}/d32_adrs_main.o" "d000000000626433325f6d61696e0000c00100000004746578740000c00200000000646174610000c0030000000062737300c00400000000737461636b00b2ff000000016162735f73796d006a010000000000010000000000000000000000000000"

# d32_adrs_abs.o
# Valid object that defines abs_sym as absolute.
write_hex_obj "${OUT_DIR}/d32_adrs_abs.o" "d0000000004c6433325f61627300c00100000000746578740000c00200000000646174610000c0030000000062737300c00400000000737461636b00b200000000006162735f73796d000000"

# stack_under.o
# Valid object stream that executes wrt_stk_9000 without push and triggers:
# "計算用スタックに値がありません"
write_hex_obj "${OUT_DIR}/stack_under.o" "d0000000004568737461636b5f756e6465720000c00100000002746578740000c00200000000646174610000c0030000000062737300c00400000000737461636b0090000000"

# stack_under_9200.o
# wrt_stk_9200 without push.
write_hex_obj "${OUT_DIR}/stack_under_9200.o" "d0000000004568737461636b5f756e6465720000c00100000002746578740000c00200000000646174610000c0030000000062737300c00400000000737461636b0092000000"

# stack_under_9600.o
# wrt_stk_9600 without push.
write_hex_obj "${OUT_DIR}/stack_under_9600.o" "d0000000004568737461636b5f756e6465720000c00100000002746578740000c00200000000646174610000c0030000000062737300c00400000000737461636b0096000000"

# stack_under_9a00.o
# wrt_stk_9a00 without push.
write_hex_obj "${OUT_DIR}/stack_under_9a00.o" "d0000000004568737461636b5f756e6465720000c00100000002746578740000c00200000000646174610000c0030000000062737300c00400000000737461636b009a000000"

# stack_over.o
# HLK's calc stack size is 1024 entries, one entry per 0x8000 push.
# Emit 1025 pushes to trigger "stack over".
write_hex_obj "${OUT_DIR}/stack_over.o" "d0000000004568737461636b5f756e6465720000c00100000002746578740000c00200000000646174610000c0030000000062737300c00400000000737461636b00"
for _ in $(seq 1 1025); do
  printf '\x80\x00\x00\x00\x00\x00' >> "${OUT_DIR}/stack_over.o"
done
printf '\x00\x00' >> "${OUT_DIR}/stack_over.o"
patch_size "${OUT_DIR}/stack_over.o"

if [[ ${#HAS_ARR[@]} -gt 0 ]]; then
  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp}"' EXIT

  cat > "${tmp}/a0_label.s" <<'ASM'
.xdef label
label equ $12345678
.end
ASM
  (cd "${tmp}" && "${HAS_ARR[@]}" -o a0_label.o a0_label.s >/dev/null 2>/dev/null)
  cp "${tmp}/a0_label.o" "${OUT_DIR}/a0_label.o"

  make_a0_main() {
    local out="$1"
    local expr="$2"
    cat > "${tmp}/${out%.o}.s" <<ASM
.xref label
.dc.l ${expr}
.end
ASM
    (cd "${tmp}" && "${HAS_ARR[@]}" -o "${out}" "${out%.o}.s" >/dev/null 2>/dev/null)
    cp "${tmp}/${out}" "${OUT_DIR}/${out}"
  }
  make_a0_main "a0_neg_main.o" ".neg.label"
  make_a0_main "a0_not_main.o" ".not.label"
  make_a0_main "a0_high_main.o" ".high.label"
  make_a0_main "a0_low_main.o" ".low.label"
  make_a0_main "a0_highw_main.o" ".highw.label"
  make_a0_main "a0_loww_main.o" ".loww.label"

  cat > "${tmp}/a0_text_label.s" <<'ASM'
.xdef label
label:
.dc.l 0
.end
ASM
  (cd "${tmp}" && "${HAS_ARR[@]}" -o a0_text_label.o a0_text_label.s >/dev/null 2>/dev/null)
  cp "${tmp}/a0_text_label.o" "${OUT_DIR}/a0_text_label.o"

  cat > "${tmp}/a0_attr_main.s" <<'ASM'
.xref label
.dc.l .neg.label
.end
ASM
  (cd "${tmp}" && "${HAS_ARR[@]}" -o a0_attr_main.o a0_attr_main.s >/dev/null 2>/dev/null)
  cp "${tmp}/a0_attr_main.o" "${OUT_DIR}/a0_attr_main.o"

  cat > "${tmp}/a0_attr_add_main.s" <<'ASM'
.xref label
.dc.l label+1
.end
ASM
  (cd "${tmp}" && "${HAS_ARR[@]}" -o a0_attr_add_main.o a0_attr_add_main.s >/dev/null 2>/dev/null)
  cp "${tmp}/a0_attr_add_main.o" "${OUT_DIR}/a0_attr_add_main.o"

  cat > "${tmp}/a0_attr_sub_main.s" <<'ASM'
.xref label
.dc.l label-1
.end
ASM
  (cd "${tmp}" && "${HAS_ARR[@]}" -o a0_attr_sub_main.o a0_attr_sub_main.s >/dev/null 2>/dev/null)
  cp "${tmp}/a0_attr_sub_main.o" "${OUT_DIR}/a0_attr_sub_main.o"

  cat > "${tmp}/stk91_main.s" <<'ASM'
.xref label
.dc.w .neg.label
.end
ASM
  (cd "${tmp}" && "${HAS_ARR[@]}" -o stk91_main.o stk91_main.s >/dev/null 2>/dev/null)
  cp "${tmp}/stk91_main.o" "${OUT_DIR}/stk91_main.o"
  cp "${tmp}/stk91_main.o" "${OUT_DIR}/stk99_main.o"
  perl -0777 -i -pe 's/\x91\x00/\x99\x00/' "${OUT_DIR}/stk99_main.o"

  cat > "${tmp}/stk93_main.s" <<'ASM'
.xref label
.dc.b .neg.label
.end
ASM
  (cd "${tmp}" && "${HAS_ARR[@]}" -o stk93_main.o stk93_main.s >/dev/null 2>/dev/null)
  cp "${tmp}/stk93_main.o" "${OUT_DIR}/stk93_main.o"

  cat > "${tmp}/stk92_main.s" <<'ASM'
.xref label
.dc.l .neg.label
.end
ASM
  (cd "${tmp}" && "${HAS_ARR[@]}" -o stk92_main.o stk92_main.s >/dev/null 2>/dev/null)
  cp "${tmp}/stk92_main.o" "${OUT_DIR}/stk96_main.o"
  cp "${tmp}/stk92_main.o" "${OUT_DIR}/stk9a_main.o"
  perl -0777 -i -pe 's/\x92\x00/\x96\x00/' "${OUT_DIR}/stk96_main.o"
  perl -0777 -i -pe 's/\x92\x00/\x9a\x00/' "${OUT_DIR}/stk9a_main.o"
fi

echo "generated fixtures in ${OUT_DIR}"
