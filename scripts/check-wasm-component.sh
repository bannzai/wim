#!/usr/bin/env bash
# Fails unless the given file is a wasm component rather than a bare core module. The two are
# told apart by the 4 bytes after the `\0asm` magic: a component carries layer 1 (0d 00 01 00),
# a core module carries the version 1 (01 00 00 00). Checking the header keeps the build
# verification free of wasm-tools, which would otherwise have to be installed just for this.
set -euo pipefail

wasm="${1:?usage: check-wasm-component.sh <file.wasm>}"

if [ ! -f "$wasm" ]; then
  echo "check-wasm-component: $wasm was not built" >&2
  exit 1
fi

header=$(od -An -tx1 -N8 "$wasm" | tr -d ' \n')
if [ "$header" != "0061736d0d000100" ]; then
  echo "check-wasm-component: $wasm is not a wasm component (header $header)" >&2
  exit 1
fi

echo "check-wasm-component: $wasm is a wasm component ($(wc -c <"$wasm" | tr -d ' ') bytes)"
