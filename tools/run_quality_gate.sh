#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"

run_step() {
  local label="$1"
  shift
  echo "==> ${label}"
  "$@"
}

run_step "clippy" \
  cargo clippy --manifest-path "${ROOT_DIR}/Cargo.toml" --all-targets --all-features -- \
    -W clippy::all -W clippy::pedantic

run_step "rust tests" \
  cargo test --manifest-path "${ROOT_DIR}/Cargo.toml"

run_step "hlk compatibility regression" \
  "${ROOT_DIR}/tools/run_hlkx_regression.sh"

echo "All quality gate checks passed."
