#!/usr/bin/env bash

run_linker() {
  local out_prefix="$1"
  local out_file="$2"
  local flags="$3"
  local objects="$4"
  shift 4
  local -a cmd=("$@")
  local stdout_file="${out_prefix}.stdout"
  local stderr_file="${out_prefix}.stderr"
  local msg_file="${out_prefix}.msg"
  local rc_file="${out_prefix}.rc"
  local map_out
  local map_out_abs
  if [[ "${out_file}" == *.* ]]; then
    map_out="${out_file%.*}.map"
  else
    map_out="${out_file}.map"
  fi
  if [[ "${map_out}" = /* ]]; then
    map_out_abs="${map_out}"
  else
    map_out_abs="${TEST_DIR}/${map_out}"
  fi

  set +e
  (
    cd "${TEST_DIR}"
    rm -f "${out_file}" "${map_out}"
    "${cmd[@]}" ${flags} -o "${out_file}" ${objects}
  ) >"${stdout_file}" 2>"${stderr_file}"
  local rc=$?
  set -e
  echo "${rc}" >"${rc_file}"
  cat "${stdout_file}" "${stderr_file}" >"${msg_file}"
  if [[ -f "${map_out_abs}" ]]; then
    if [[ "${map_out_abs}" != "${out_prefix}.map" ]]; then
      cp "${map_out_abs}" "${out_prefix}.map"
    fi
  else
    rm -f "${out_prefix}.map"
  fi
}

compare_case() {
  local name="$1"
  local ext="$2"
  local orig_prefix="${ARTIFACT_DIR}/orig/${name}"
  local rhlk_prefix="${ARTIFACT_DIR}/rhlk/${name}"
  local diff_file="${ARTIFACT_DIR}/diff/${name}.diff"
  local orig_norm="${orig_prefix}.msg.norm"
  local rhlk_norm="${rhlk_prefix}.msg.norm"
  local orig_map_norm="${orig_prefix}.map.norm"
  local rhlk_map_norm="${rhlk_prefix}.map.norm"
  local orig_map_raw="${orig_prefix}.map.rawcmp"
  local rhlk_map_raw="${rhlk_prefix}.map.rawcmp"
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
  local orig_rc rhlk_rc
  orig_rc="$(cat "${orig_prefix}.rc")"
  rhlk_rc="$(cat "${rhlk_prefix}.rc")"
  if [[ "${orig_rc}" == "0" && "${rhlk_rc}" == "0" && ( -f "${orig_map}" || -f "${rhlk_map}" ) ]]; then
    if [[ ! -f "${orig_map}" || ! -f "${rhlk_map}" ]]; then
      echo "[${name}] map output existence differs" >>"${diff_file}"
      failed=1
    else
      normalize_map_raw "${orig_map}" "${orig_map_raw}"
      normalize_map_raw "${rhlk_map}" "${rhlk_map_raw}"
      if ! diff -u "${orig_map_raw}" "${rhlk_map_raw}" >>"${diff_file}" 2>&1; then
        echo "[${name}] map output differs (raw format, path-normalized)" >>"${diff_file}"
        failed=1
      fi
      normalize_map "${orig_map}" "${orig_map_norm}"
      normalize_map "${rhlk_map}" "${rhlk_map_norm}"
      if ! diff -u "${orig_map_norm}" "${rhlk_map_norm}" >>"${diff_file}" 2>&1; then
        echo "[${name}] map output differs (normalized symbols)" >>"${diff_file}"
        failed=1
      fi
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
