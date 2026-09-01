#!/usr/bin/env bash
# Puts a pinned wasm-tools binary at the path this script prints.
#
# The plugin is built for wasm32-unknown-unknown and turned into a component afterwards, and
# wasm-tools is the tool that does the turning. A release binary is downloaded rather than
# `cargo install`ed because the install builds for minutes on every cold CI run; the version is
# pinned so that every environment encodes the component the same way.
set -euo pipefail

# 1.258.0 is the version the ABI was verified against when wit/ was written.
VERSION="1.258.0"
DESTINATION="${1:-target/tools}"
BINARY="$DESTINATION/wasm-tools-$VERSION/wasm-tools"

if [ -x "$BINARY" ]; then
    echo "$BINARY"
    exit 0
fi

case "$(uname -s)-$(uname -m)" in
Linux-x86_64) TARGET="x86_64-linux" ;;
Linux-aarch64) TARGET="aarch64-linux" ;;
Darwin-x86_64) TARGET="x86_64-macos" ;;
Darwin-arm64) TARGET="aarch64-macos" ;;
*)
    echo "unsupported platform: $(uname -s)-$(uname -m)" >&2
    exit 1
    ;;
esac

ARCHIVE="wasm-tools-$VERSION-$TARGET.tar.gz"
mkdir -p "$DESTINATION"
curl --fail --location --silent --show-error \
    "https://github.com/bytecodealliance/wasm-tools/releases/download/v$VERSION/$ARCHIVE" |
    tar -xz -C "$DESTINATION"
mv "$DESTINATION/wasm-tools-$VERSION-$TARGET" "$DESTINATION/wasm-tools-$VERSION"
echo "$BINARY"
