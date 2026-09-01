#!/usr/bin/env bash
# Turns built plugin components into the ES modules the browser demo imports.
#
# `jco transpile` is the browser half of what wasmtime does natively: it reads the same .wasm
# component and writes JS that lowers the canonical ABI, plus the core module the JS instantiates.
# Nothing it writes is committed — the source is `plugins/` in this repo and the output is
# reproduced from it on every build, which is what tells it apart from the tree-sitter runtime
# under `web/vendor/`, whose source is not here.
#
# Usage: transpile-plugins.sh <name>=<component.wasm> [...]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Where the demo looks for them. `web/plugins.js` names the same directory in its manifest URL.
OUT="$ROOT/web/plugins"
JCO="$ROOT/web/node_modules/.bin/jco"
# The wit is the one source of the ABI version, here as in `wim-plugin-host`: the host reads it
# off the package line rather than holding a constant that could drift away from it.
WIT="$ROOT/wit/plugin.wit"

if [ "$#" -eq 0 ]; then
    echo "usage: $(basename "$0") <name>=<component.wasm> [...]" >&2
    exit 1
fi
if [ ! -x "$JCO" ]; then
    echo "$JCO is not there; run \`npm ci\` in web/ first" >&2
    exit 1
fi

ABI="$(sed -n 's/^package wim:plugin@\(.*\);$/\1/p' "$WIT")"
if [ -z "$ABI" ]; then
    echo "$WIT does not declare a versioned wim:plugin package" >&2
    exit 1
fi

# Cleared rather than written over, so that a plugin dropped from the build leaves nothing of
# itself behind in what the demo is served.
rm -rf "$OUT"
mkdir -p "$OUT"

ENTRIES=""
for PLUGIN in "$@"; do
    NAME="${PLUGIN%%=*}"
    COMPONENT="${PLUGIN#*=}"
    if [ "$NAME" = "$PLUGIN" ] || [ -z "$NAME" ] || [ -z "$COMPONENT" ]; then
        echo "not a <name>=<component.wasm> pair: $PLUGIN" >&2
        exit 1
    fi
    bash "$ROOT/scripts/check-wasm-component.sh" "$COMPONENT"
    # --base64-cutoff 0 keeps the core module a file of its own whatever its size, so that a
    # small component and a large one are served the same way and what is checked locally is the
    # shape CI runs. The demo's server and GitHub Pages both send .wasm as application/wasm.
    "$JCO" transpile "$COMPONENT" --out-dir "$OUT/$NAME" --name "$NAME" --base64-cutoff 0 --quiet

    # The sandbox, checked where it is decided. The world imports `wim:plugin/buffer`, which
    # carries types and no functions, so jco has nothing to import and writes a module with no
    # imports at all — no WASI shim, no host functions. An import appearing here would mean the
    # browser is being handed a capability the native host's empty linker refuses.
    if grep -nE "^import[ {*]" "$OUT/$NAME/$NAME.js"; then
        echo "$NAME.js imports something; the browser host provides nothing to import" >&2
        exit 1
    fi

    [ -z "$ENTRIES" ] || ENTRIES="$ENTRIES,"
    ENTRIES="$ENTRIES
    { \"name\": \"$NAME\", \"module\": \"./plugins/$NAME/$NAME.js\" }"
done

# What the demo reads to find out which plugins were built and which ABI to hold them to.
cat >"$OUT/manifest.json" <<JSON
{
  "abi": "$ABI",
  "plugins": [$ENTRIES
  ]
}
JSON
echo "transpiled $# plugin(s) for wim:plugin@$ABI into $OUT"
