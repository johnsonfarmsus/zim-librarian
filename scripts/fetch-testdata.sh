#!/bin/sh
# Fetch the ZIM files the test-suite runs against (gitignored, ~9 MB total).
set -e
cd "$(dirname "$0")/../testdata"
base=https://raw.githubusercontent.com/openzim/zim-testing-suite/main/data
[ -f small.zim ]       || curl -sL -o small.zim       "$base/withns/small.zim"
[ -f small_nons.zim ]  || curl -sL -o small_nons.zim  "$base/nons/small.zim"
[ -f alpinelinux.zim ] || curl -sL -o alpinelinux.zim "https://download.kiwix.org/zim/other/alpinelinux_en_all_maxi_2026-04.zim"
[ -f cyborg.zim ]      || curl -sL -o cyborg.zim      "https://download.kiwix.org/zim/other/cyborganthropology.com_en_all_maxi_2026-05.zim"
echo "testdata ready:" && ls -la
