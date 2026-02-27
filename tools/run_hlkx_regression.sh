#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
TEST_DIR="${ROOT_DIR}/external/hlkx/tests"
CASE_FILE="${ROOT_DIR}/tests/compat/hlkx_cases.tsv"
FIXTURE_DIR="${ROOT_DIR}/tests/compat/fixtures"
ARTIFACT_DIR="${ROOT_DIR}/artifacts/hlkx-regression"
RUN68_SUBMODULE_BIN="${ROOT_DIR}/external/run68x/build/run68"
TOOLCHAIN_BIN_DIR="${ROOT_DIR}/external/toolchain/bin"
HAS_BIN_DEFAULT="${TOOLCHAIN_BIN_DIR}/has060x.x"
HLK_BIN_DEFAULT="${TOOLCHAIN_BIN_DIR}/hlkx.r"

# Override these if your environment differs.
if [[ -x "${RUN68_SUBMODULE_BIN}" ]]; then
  RUN68_CMD_DEFAULT="${RUN68_SUBMODULE_BIN}"
else
  RUN68_CMD_DEFAULT="run68"
fi
HAS_CMD_DEFAULT="${RUN68_CMD_DEFAULT} has060.x"
HLK_CMD_DEFAULT="${RUN68_CMD_DEFAULT} ${ROOT_DIR}/external/hlkx/build/hlk.x"
RHLK_CMD_DEFAULT="cargo run --manifest-path ${ROOT_DIR}/Rhlk/Cargo.toml --quiet --"

if [[ -f "${HAS_BIN_DEFAULT}" ]]; then
  HAS_CMD_DEFAULT="${RUN68_CMD_DEFAULT} ${HAS_BIN_DEFAULT}"
fi
if [[ -f "${HLK_BIN_DEFAULT}" ]]; then
  HLK_CMD_DEFAULT="${RUN68_CMD_DEFAULT} ${HLK_BIN_DEFAULT}"
fi

HAS_CMD="${HAS_CMD:-$HAS_CMD_DEFAULT}"
HLK_CMD="${HLK_CMD:-$HLK_CMD_DEFAULT}"
RHLK_CMD="${RHLK_CMD:-$RHLK_CMD_DEFAULT}"

read -r -a HAS_ARR <<<"${HAS_CMD}"
read -r -a HLK_ARR <<<"${HLK_CMD}"
read -r -a RHLK_ARR <<<"${RHLK_CMD}"

mkdir -p "${ARTIFACT_DIR}/orig" "${ARTIFACT_DIR}/rhlk" "${ARTIFACT_DIR}/diff"

require_cmd() {
  local cmd="$1"
  if ! command -v "${cmd}" >/dev/null 2>&1; then
    echo "missing command: ${cmd}" >&2
    return 1
  fi
  return 0
}

require_inputs() {
  local missing=0
  if [[ ! -f "${ROOT_DIR}/external/hlkx/build/hlk.x" && ! -f "${HLK_BIN_DEFAULT}" ]]; then
    echo "missing linker binary: ${ROOT_DIR}/external/hlkx/build/hlk.x or ${HLK_BIN_DEFAULT}" >&2
    echo "build HLKX or run tools/setup_human68k_binaries.sh." >&2
    missing=1
  fi
  if [[ ! -f "${HAS_BIN_DEFAULT}" ]]; then
    if ! command -v has060.x >/dev/null 2>&1; then
      echo "missing assembler command: has060.x or ${HAS_BIN_DEFAULT}" >&2
      echo "run tools/setup_human68k_binaries.sh or install has060.x in PATH." >&2
      missing=1
    fi
  fi
  return "${missing}"
}

cleanup_test_objects() {
  while IFS=$'\t' read -r name _flags objects _ext; do
    [[ -z "${name}" || "${name:0:1}" == "#" ]] && continue
    for obj in ${objects}; do
      rm -f "${TEST_DIR}/${obj}"
    done
  done <"${CASE_FILE}"
}

assemble_if_needed() {
  local src="$1"
  local obj="$2"
  local src_path="${TEST_DIR}/${src}"
  local fixture="${FIXTURE_DIR}/${obj}"
  if [[ -f "${src_path}" ]]; then
    if [[ ! -f "${TEST_DIR}/${obj}" || "${src_path}" -nt "${TEST_DIR}/${obj}" ]]; then
      (cd "${TEST_DIR}" && "${HAS_ARR[@]}" -o "${obj}" "${src}")
    fi
    return 0
  fi
  if [[ -f "${fixture}" ]]; then
    cp "${fixture}" "${TEST_DIR}/${obj}"
    return 0
  fi
  echo "missing source and fixture for ${obj}" >&2
  return 1
}

run_linker() {
  local tag="$1"
  local out_prefix="$2"
  local out_file="$3"
  local flags="$4"
  local objects="$5"
  shift 5
  local -a cmd=("$@")
  local stdout_file="${out_prefix}.stdout"
  local stderr_file="${out_prefix}.stderr"
  local msg_file="${out_prefix}.msg"
  local rc_file="${out_prefix}.rc"
  local map_out
  if [[ "${out_file}" == *.* ]]; then
    map_out="${out_file%.*}.map"
  else
    map_out="${out_file}.map"
  fi

  set +e
  (
    cd "${TEST_DIR}"
    rm -f "${out_file}"
    rm -f "${map_out}"
    "${cmd[@]}" ${flags} -o "${out_file}" ${objects}
  ) >"${stdout_file}" 2>"${stderr_file}"
  local rc=$?
  set -e
  echo "${rc}" >"${rc_file}"
  cat "${stdout_file}" "${stderr_file}" >"${msg_file}"
  if [[ -f "${TEST_DIR}/${map_out}" ]]; then
    cp "${TEST_DIR}/${map_out}" "${out_prefix}.map"
  else
    rm -f "${out_prefix}.map"
  fi
  return 0
}

compare_case() {
  local name="$1"
  local ext="$2"
  local orig_prefix="${ARTIFACT_DIR}/orig/${name}"
  local rhlk_prefix="${ARTIFACT_DIR}/rhlk/${name}"
  local diff_file="${ARTIFACT_DIR}/diff/${name}.diff"
  local orig_norm="${orig_prefix}.msg.norm"
  local rhlk_norm="${rhlk_prefix}.msg.norm"
  : >"${diff_file}"

  normalize_msg "${orig_prefix}.msg" "${orig_norm}" "orig"
  normalize_msg "${rhlk_prefix}.msg" "${rhlk_norm}" "rhlk"

  local failed=0
  if ! diff -u "${orig_norm}" "${rhlk_norm}" >>"${diff_file}" 2>&1; then
    echo "[${name}] merged message differs" >>"${diff_file}"
    failed=1
  fi
  if ! diff -u "${orig_prefix}.rc" "${rhlk_prefix}.rc" >>"${diff_file}" 2>&1; then
    echo "[${name}] exit code differs" >>"${diff_file}"
    failed=1
  fi

  local orig_out="${orig_prefix}.${ext}"
  local rhlk_out="${rhlk_prefix}.${ext}"
  if [[ -f "${orig_out}" || -f "${rhlk_out}" ]]; then
    if [[ ! -f "${orig_out}" || ! -f "${rhlk_out}" ]]; then
      echo "[${name}] output existence differs (${ext})" >>"${diff_file}"
      failed=1
    elif ! cmp -s "${orig_out}" "${rhlk_out}"; then
      echo "[${name}] output binary differs (${ext})" >>"${diff_file}"
      failed=1
    fi
  fi

  local orig_map="${orig_prefix}.map"
  local rhlk_map="${rhlk_prefix}.map"
  if [[ -f "${orig_map}" || -f "${rhlk_map}" ]]; then
    if [[ ! -f "${orig_map}" || ! -f "${rhlk_map}" ]]; then
      echo "[${name}] map output existence differs" >>"${diff_file}"
      failed=1
    elif ! cmp -s "${orig_map}" "${rhlk_map}"; then
      echo "[${name}] map output differs" >>"${diff_file}"
      failed=1
    fi
  fi

  if [[ ${failed} -eq 0 ]]; then
    rm -f "${diff_file}"
    echo "PASS ${name}"
  else
    echo "FAIL ${name} (see ${diff_file})"
  fi
  return "${failed}"
}

normalize_msg() {
  local input="$1"
  local output="$2"
  local kind="$3"
  local tmp="${output}.tmp"
  if [[ "${kind}" == "orig" ]]; then
    iconv -f SHIFT_JIS -t UTF-8 -c "${input}" >"${tmp}" || cp "${input}" "${tmp}"
  else
    cp "${input}" "${tmp}"
  fi
  sed -E \
    -e 's/\r$//' \
    -e 's/^Error: //' \
    -e 's#^ at [0-9A-Fa-f]{8} \((text|data|rdata|rldata)\)$# at <ADDR> (\1)#' \
    -e 's#(実行開始アドレスがファイル先頭ではありません:).*#\1 <PATH>#' \
    -e 's#(再配置テーブルが使われています:).*#\1 <PATH>#' \
    -e 's#(再配置対象が奇数アドレスにあります:).*#\1 <PATH>#' \
    -e 's#(MACS形式ファイルではありません:).*#\1 <PATH>#' \
    "${tmp}" >"${output}"
  rm -f "${tmp}"
}

main() {
  trap cleanup_test_objects EXIT
  require_cmd cargo
  require_inputs

  local failed=0
  while IFS=$'\t' read -r name flags objects ext; do
    [[ -z "${name}" || "${name:0:1}" == "#" ]] && continue
    [[ "${flags}" == "-" ]] && flags=""

    for src in ${objects//.o/.s}; do
      assemble_if_needed "${src}" "${src%.s}.o"
    done

    local orig_prefix="${ARTIFACT_DIR}/orig/${name}"
    local rhlk_prefix="${ARTIFACT_DIR}/rhlk/${name}"
    run_linker "orig" "${orig_prefix}" "${orig_prefix}.${ext}" "${flags}" "${objects}" "${HLK_ARR[@]}"
    run_linker "rhlk" "${rhlk_prefix}" "${rhlk_prefix}.${ext}" "${flags}" "${objects}" "${RHLK_ARR[@]}"

    if ! compare_case "${name}" "${ext}"; then
      failed=1
    fi
  done <"${CASE_FILE}"

  if [[ ${failed} -ne 0 ]]; then
    echo "Regression mismatch detected."
    exit 1
  fi
  echo "All regression cases matched."
}

main "$@"
