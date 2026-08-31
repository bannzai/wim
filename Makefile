WASM := target/wasm32-unknown-unknown/release/wim_wasm.wasm

.PHONY: build-web web e2e install-wasm-bindgen

# Builds the wasm module and the JS glue the demo page imports.
build-web:
	cargo build -p wim-wasm --target wasm32-unknown-unknown --release
	wasm-bindgen $(WASM) --out-dir web/pkg --target web

# Builds the demo and serves it at http://127.0.0.1:4173/.
web: build-web
	node web/serve.mjs

# Runs the browser E2E against a freshly built demo.
e2e: build-web
	cd web && npm ci && npx playwright install --with-deps chromium && npx playwright test

# wasm-bindgen-cli has to match the crate version, which Cargo.lock holds.
install-wasm-bindgen:
	cargo install wasm-bindgen-cli --version "$$(bash scripts/wasm-bindgen-version.sh)" --locked
