#!/usr/bin/env bash
# Puts the tree-sitter runtime and the grammars the demo highlights with into
# web/vendor/tree-sitter/, where the page loads them from.
#
# The files this installs are committed rather than fetched at build time. `web/` is what the
# Pages workflow uploads as the site — it runs no npm and no fetch of its own — so a grammar that
# had to be downloaded would have to be downloaded again in the Pages job and in the E2E job, and
# the demo would go dark the day one of those downloads failed. Committing them makes the deploy
# and the test run read the same bytes this script verified.
#
# Adding a language is a row in GRAMMARS here and a row in LANGUAGES in web/highlight.js.
#
#   bash scripts/vendor-tree-sitter.sh          # fetch, verify and install
#   bash scripts/vendor-tree-sitter.sh --check  # verify what is committed, fetching nothing

set -euo pipefail

# The tree-sitter runtime the page imports. web-tree-sitter and a grammar agree on a parser ABI
# rather than on a version, so the pin that matters is the one this script records checksums for:
# 0.27.0 reads ABI 13 to 15, and both grammars below are built at 15.
readonly RUNTIME_VERSION=0.27.0

# `<id>|<repo>|<tag>|<wasm asset>|<highlights.scm inside the source tarball>`, where `<id>` is the
# name web/highlight.js loads the grammar under. The wasm and the query come from the one release
# so that the query is the one written against that grammar's node names.
readonly GRAMMARS=(
  "rust|tree-sitter/tree-sitter-rust|v0.24.2|tree-sitter-rust.wasm|queries/highlights.scm"
  "markdown|tree-sitter-grammars/tree-sitter-markdown|v0.5.3|tree-sitter-markdown.wasm|tree-sitter-markdown/queries/highlights.scm"
)

readonly ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly VENDOR="$ROOT/web/vendor/tree-sitter"
readonly CHECKSUMS="$ROOT/scripts/tree-sitter-vendor.sha256"

# macOS ships `shasum`, the GitHub runners' Linux images ship `sha256sum`, and the two read and
# write the same format.
if command -v shasum >/dev/null 2>&1; then
  sha256() { shasum -a 256 "$@"; }
else
  sha256() { sha256sum "$@"; }
fi

# Verifies the installed files against the recorded checksums, which is what tells a stale or a
# half-written vendor directory from the one this script last wrote.
check() {
  (cd "$VENDOR" && sha256 -c "$CHECKSUMS")
}

if [[ "${1:-}" == "--check" ]]; then
  check
  exit 0
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

mkdir -p "$VENDOR"

# The runtime comes from the npm registry because that is the only place it is published; the
# grammars come from their GitHub releases, which is where their built wasm is.
curl --fail --silent --show-error --location \
  --output "$work/runtime.tgz" \
  "https://registry.npmjs.org/web-tree-sitter/-/web-tree-sitter-$RUNTIME_VERSION.tgz"
tar xzf "$work/runtime.tgz" -C "$work" \
  package/web-tree-sitter.js package/web-tree-sitter.wasm package/LICENSE
install -m 644 "$work/package/web-tree-sitter.js" "$VENDOR/web-tree-sitter.js"
install -m 644 "$work/package/web-tree-sitter.wasm" "$VENDOR/web-tree-sitter.wasm"
install -m 644 "$work/package/LICENSE" "$VENDOR/LICENSE-web-tree-sitter"

for grammar in "${GRAMMARS[@]}"; do
  IFS='|' read -r id repo tag asset query <<<"$grammar"
  release="https://github.com/$repo/releases/download/$tag"
  source_dir="$work/$id"
  mkdir -p "$source_dir"
  curl --fail --silent --show-error --location \
    --output "$VENDOR/$id.wasm" "$release/$asset"
  curl --fail --silent --show-error --location \
    --output "$work/$id.tar.gz" "$release/${asset%.wasm}.tar.gz"
  tar xzf "$work/$id.tar.gz" -C "$source_dir" "$query" LICENSE
  install -m 644 "$source_dir/$query" "$VENDOR/$id.highlights.scm"
  install -m 644 "$source_dir/LICENSE" "$VENDOR/LICENSE-tree-sitter-$id"
  chmod 644 "$VENDOR/$id.wasm"
done

(cd "$VENDOR" && sha256 -- * | sort -k2 >"$CHECKSUMS")
check
echo "installed into $VENDOR"
