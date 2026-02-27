#!/usr/bin/env bash

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

normalize_map() {
  local input="$1"
  local output="$2"
  perl -ne '
    s/\r$//;
    if (/^([0-9A-Fa-f]{8})\s+([A-Za-z]+)\s+(.+)$/) {
      my ($addr, $sect, $name) = (uc($1), lc($2), $3);
      $name =~ s/^\s+|\s+$//g;
      next if $name eq "" || $sect eq "unknown";
      print "$name\t$addr\t$sect\n";
      next;
    }
    if (/^\s*([^ \t][^:]*)\s*:\s*([0-9A-Fa-f]{8})\s*\(([A-Za-z ]+)\)/) {
      my ($name, $addr, $sect) = ($1, uc($2), lc($3));
      $name =~ s/^\s+|\s+$//g;
      $sect =~ s/\s+//g;
      next if $name eq "" || $sect eq "";
      print "$name\t$addr\t$sect\n";
    }
  ' "${input}" | LC_ALL=C sort -u >"${output}"
}

normalize_map_raw() {
  local input="$1"
  local output="$2"
  # Compare full map formatting while ignoring only the executable path line.
  # line2 is:
  #   A:\...\<name>.x
  perl -ne '
    s/\r$//;
    if ($. == 2) {
      s#^A:.*[\\/]([^\\/]+\.x)$#A:<PATH>\\$1#i;
    }
    print "$_\n";
  ' "${input}" >"${output}"
}
