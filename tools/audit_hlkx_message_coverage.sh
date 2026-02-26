#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
ARTIFACT_DIR="${ROOT_DIR}/artifacts/hlkx-regression/orig"

if [[ ! -d "${ARTIFACT_DIR}" ]]; then
  echo "missing ${ARTIFACT_DIR}. run ./tools/run_hlkx_regression.sh first." >&2
  exit 1
fi

patterns=(
  'アドレス属性シンボルの値をバイトサイズで出力 in '
  'アドレス属性シンボルの値をワードサイズで出力 in '
  '32ビットディスプレースメントにアドレス属性シンボルの値を出力 in '
  'ゼロ除算 in '
  '不正な式 in '
  'バイトサイズ(-$80〜$ff)で表現できない値 in '
  'バイトサイズ(-$80〜$7f)で表現できない値 in '
  'ワードサイズ(-$8000〜$ffff)で表現できない値 in '
  'ワードサイズ(-$8000〜$7fff)で表現できない値 in '
  '計算用スタックが溢れました in '
  '計算用スタックに値がありません in '
  '複数の実行開始アドレスを指定することはできません in '
  '再配置テーブルが使われています: '
  '再配置対象が奇数アドレスにあります: '
  '実行開始アドレスがファイル先頭ではありません: '
  'MACS形式ファイルではありません: '
)

for p in "${patterns[@]}"; do
  hits="$(grep -R -l -- "${p}" "${ARTIFACT_DIR}"/*.msg.norm 2>/dev/null || true)"
  if [[ -n "${hits}" ]]; then
    files="$(printf "%s\n" "${hits}" | xargs -n1 basename | sed -E 's/\.msg\.norm$//' | paste -sd, -)"
    echo "COVERED | ${p} | ${files}"
  else
    echo "UNCOVERED | ${p}"
  fi
done
