#!/bin/zsh
set -eu
cd -- "${0:A:h}"
export PILLOWGUARD_HOME="$PWD"
if [[ -x ./bin/pillowguard-server ]]; then
  exec ./bin/pillowguard-server
else
  echo '请先构建 Rust 程序：cargo build --release --locked'
  exit 1
fi
