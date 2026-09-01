WASM := target/wasm32-unknown-unknown/release/wim_wasm.wasm

.PHONY: build-web web e2e install-wasm-bindgen vendor-tree-sitter

# Builds the wasm module and the JS glue the demo page imports.
build-web:
	cargo build -p wim-wasm --target wasm32-unknown-unknown --release
	wasm-bindgen $(WASM) --out-dir web/pkg --target web

# Builds the demo and serves it at http://127.0.0.1:4173/.
web: build-web
	node web/serve.mjs

# Runs the browser E2E against a freshly built demo. The daemon is built too: the file-access
# run starts it over a directory of its own and drives the demo against it.
e2e: build-web
	cargo build -p wim
	cd web && npm ci && npx playwright install --with-deps chromium && npx playwright test

# Refetches the tree-sitter runtime and the grammars the demo highlights with. What it installs is
# committed under web/vendor/, so this is only run to move a pinned version.
vendor-tree-sitter:
	bash scripts/vendor-tree-sitter.sh

# wasm-bindgen-cli has to match the crate version, which Cargo.lock holds.
install-wasm-bindgen:
	cargo install wasm-bindgen-cli --version "$$(bash scripts/wasm-bindgen-version.sh)" --locked
