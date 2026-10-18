#!/bin/zsh
set -eu
cd -- "${0:A:h}"
export ZHENAN_HOME="$PWD"
if [[ -x ./bin/zhenan-server ]]; then
  exec ./bin/zhenan-server
else
  echo '请先构建 Rust 程序：cargo build --release --locked'
  exit 1
fi
