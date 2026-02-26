#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
TEST_DIR="${ROOT_DIR}/external/hlkx/tests"
CASE_FILE="${ROOT_DIR}/tests/compat/hlkx_cases.tsv"
ARTIFACT_DIR="${ROOT_DIR}/artifacts/hlkx-regression"

# Override these if your environment differs.
HAS_CMD_DEFAULT="run68 has060.x"
HLK_CMD_DEFAULT="run68 ${ROOT_DIR}/external/hlkx/build/hlk.x"
RHLK_CMD_DEFAULT="cargo run --manifest-path ${ROOT_DIR}/Rhlk/Cargo.toml --quiet --"

HAS_CMD="${HAS_CMD:-$HAS_CMD_DEFAULT}"
HLK_CMD="${HLK_CMD:-$HLK_CMD_DEFAULT}"
RHLK_CMD="${RHLK_CMD:-$RHLK_CMD_DEFAULT}"

read -r -a HAS_ARR <<<"${HAS_CMD}"
read -r -a HLK_ARR <<<"${HLK_CMD}"
read -r -a RHLK_ARR <<<"${RHLK_CMD}"

mkdir -p "${ARTIFACT_DIR}/orig" "${ARTIFACT_DIR}/rhlk" "${ARTIFACT_DIR}/diff"

assemble_if_needed() {
  local src="$1"
  local obj="$2"
  if [[ ! -f "${obj}" || "${src}" -nt "${obj}" ]]; then
    (cd "${TEST_DIR}" && "${HAS_ARR[@]}" -o "${obj}" "${src}")
  fi
}

run_linker() {
  local tag="$1"
  local out="$2"
  local flags="$3"
  local objects="$4"
  shift 4
  local -a cmd=("$@")
  local stdout_file="${out}.stdout"
  local stderr_file="${out}.stderr"
  local rc_file="${out}.rc"

  set +e
  (
    cd "${TEST_DIR}"
    "${cmd[@]}" ${flags} -o "${out}" ${objects}
  ) >"${stdout_file}" 2>"${stderr_file}"
  local rc=$?
  set -e
  echo "${rc}" >"${rc_file}"
  return 0
}

compare_case() {
  local name="$1"
  local ext="$2"
  local orig_prefix="${ARTIFACT_DIR}/orig/${name}"
  local rhlk_prefix="${ARTIFACT_DIR}/rhlk/${name}"
  local diff_file="${ARTIFACT_DIR}/diff/${name}.diff"
  : >"${diff_file}"

  local failed=0
  if ! diff -u "${orig_prefix}.stdout" "${rhlk_prefix}.stdout" >>"${diff_file}" 2>&1; then
    echo "[${name}] stdout differs" >>"${diff_file}"
    failed=1
  fi
  if ! diff -u "${orig_prefix}.stderr" "${rhlk_prefix}.stderr" >>"${diff_file}" 2>&1; then
    echo "[${name}] stderr differs" >>"${diff_file}"
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

  if [[ ${failed} -eq 0 ]]; then
    rm -f "${diff_file}"
    echo "PASS ${name}"
  else
    echo "FAIL ${name} (see ${diff_file})"
  fi
  return "${failed}"
}

main() {
  local failed=0
  while IFS=$'\t' read -r name flags objects ext; do
    [[ -z "${name}" || "${name:0:1}" == "#" ]] && continue

    for src in ${objects//.o/.s}; do
      assemble_if_needed "${src}" "${src%.s}.o"
    done

    local orig_prefix="${ARTIFACT_DIR}/orig/${name}"
    local rhlk_prefix="${ARTIFACT_DIR}/rhlk/${name}"
    run_linker "orig" "${orig_prefix}.${ext}" "${flags}" "${objects}" "${HLK_ARR[@]}"
    run_linker "rhlk" "${rhlk_prefix}.${ext}" "${flags}" "${objects}" "${RHLK_ARR[@]}"

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
