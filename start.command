#!/bin/zsh
set -eu
cd -- "${0:A:h}"
export LAOREN_HOME="$PWD"
if [[ -x ./bin/laoren-server ]]; then
  exec ./bin/laoren-server
else
  echo '请先构建 Rust 程序：cargo build --release --locked'
  exit 1
fi
