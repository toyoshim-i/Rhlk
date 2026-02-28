#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
source "${ROOT_DIR}/tools/lib/regression_normalize.sh"
source "${ROOT_DIR}/tools/lib/regression_case.sh"
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
RHLK_CMD_DEFAULT="cargo run --manifest-path ${ROOT_DIR}/Cargo.toml --quiet --"

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
    run_linker "${orig_prefix}" "${orig_prefix}.${ext}" "${flags}" "${objects}" "${HLK_ARR[@]}"
    run_linker "${rhlk_prefix}" "${rhlk_prefix}.${ext}" "${flags}" "${objects}" "${RHLK_ARR[@]}"

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
