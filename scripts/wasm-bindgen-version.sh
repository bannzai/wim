#!/usr/bin/env bash
# Prints the version of the wasm-bindgen crate the workspace is locked to.
#
# wasm-bindgen-cli refuses a wasm module built against a different version of the crate, so
# the version is read out of Cargo.lock rather than written down a second time here.
set -euo pipefail

awk '
    /^name = "wasm-bindgen"$/ { found = 1; next }
    found && /^version = / { gsub(/[",]/, "", $3); print $3; exit }
' "$(dirname "$0")/../Cargo.lock"
