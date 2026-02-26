#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN_DIR="${ROOT_DIR}/external/toolchain/bin"
TMP_DIR="${ROOT_DIR}/artifacts/toolchain-download"

mkdir -p "${BIN_DIR}" "${TMP_DIR}"

download_and_extract() {
  local url="$1"
  local zip_name="$2"
  local pattern="$3"
  local out_name="$4"

  local zip_path="${TMP_DIR}/${zip_name}"
  curl -fL -o "${zip_path}" "${url}"

  local picked
  picked="$(unzip -Z1 "${zip_path}" | rg -i "${pattern}" | head -n 1 || true)"
  if [[ -z "${picked}" ]]; then
    echo "no matching entry (${pattern}) in ${zip_name}" >&2
    return 1
  fi
  unzip -p "${zip_path}" "${picked}" > "${BIN_DIR}/${out_name}"
}

download_and_extract \
  "https://github.com/kg68k/hlkx/releases/download/v1.1.0/hlkx110.zip" \
  "hlkx110.zip" \
  '^hlkx\.r$' \
  "hlkx.r"

download_and_extract \
  "https://github.com/kg68k/has060xx/releases/download/v1.2.5/hasx125.zip" \
  "hasx125.zip" \
  '^has060x\.x$' \
  "has060x.x"

download_and_extract \
  "https://github.com/kg68k/u8tosj/releases/download/1.0.1/u8tosj101.zip" \
  "u8tosj101.zip" \
  '^u8tosj\.r$' \
  "u8tosj.r"

echo "installed binaries:"
ls -la "${BIN_DIR}"
