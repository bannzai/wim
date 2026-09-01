WASM := target/wasm32-unknown-unknown/release/wim_wasm.wasm
PLUGINS := plugins/Cargo.toml
HELLO_WIM_CORE := plugins/target/wasm32-unknown-unknown/release/hello_wim.wasm
HELLO_WIM := plugins/target/wasm32-unknown-unknown/release/hello_wim.component.wasm
MARKDOWN_PREVIEW_CORE := plugins/target/wasm32-unknown-unknown/release/markdown_preview.wasm
MARKDOWN_PREVIEW := plugins/target/wasm32-unknown-unknown/release/markdown_preview.component.wasm

.PHONY: build-web build-web-plugins web e2e install-wasm-bindgen vendor-tree-sitter build-plugins check-plugins test-plugin-host

# Builds the wasm module and the JS glue the demo page imports.
build-web:
	cargo build -p wim-wasm --target wasm32-unknown-unknown --release
	wasm-bindgen $(WASM) --out-dir web/pkg --target web

# Transpiles the first-party plugins into the ES modules the demo imports, which is the browser's
# half of what `make test-plugin-host` does natively over the same components. What jco writes is
# generated on every build and gitignored, so this runs wherever the demo is built rather than
# being committed the way the tree-sitter runtime is.
build-web-plugins: build-plugins
	cd web && npm ci
	bash scripts/transpile-plugins.sh \
		hello-wim="$(CURDIR)/$(HELLO_WIM)" \
		markdown-preview="$(CURDIR)/$(MARKDOWN_PREVIEW)"

# Builds the demo and serves it at http://127.0.0.1:4173/.
web: build-web build-web-plugins
	node web/serve.mjs

# Runs the browser E2E against a freshly built demo. The daemon is built too: the file-access
# run starts it over a directory of its own and drives the demo against it, and the plugin run
# calls it over the very component the demo was given a transpile of.
e2e: build-web build-web-plugins
	cargo build -p wim
	cd web && npx playwright install --with-deps chromium && \
		WIM_PLUGIN_WASM="$(CURDIR)/$(HELLO_WIM)" \
		WIM_MARKDOWN_PREVIEW_WASM="$(CURDIR)/$(MARKDOWN_PREVIEW)" npx playwright test

# Refetches the tree-sitter runtime and the grammars the demo highlights with. What it installs is
# committed under web/vendor/, so this is only run to move a pinned version.
vendor-tree-sitter:
	bash scripts/vendor-tree-sitter.sh

# wasm-bindgen-cli has to match the crate version, which Cargo.lock holds.
install-wasm-bindgen:
	cargo install wasm-bindgen-cli --version "$$(bash scripts/wasm-bindgen-version.sh)" --locked

# Builds the first-party plugins as wasm components. The build targets wasm32-unknown-unknown
# rather than wasm32-wasip2 because wasip2's std links WASI imports (wasi:io and friends) into the
# component, and the host's sandbox refuses everything that imports WASI; on unknown-unknown the
# component imports nothing but the ABI's own types. The core modules wit-bindgen leaves behind are
# turned into components by a pinned wasm-tools binary.
build-plugins:
	cargo build --manifest-path $(PLUGINS) --target wasm32-unknown-unknown --release --locked
	bash scripts/install-wasm-tools.sh > /dev/null
	"$$(bash scripts/install-wasm-tools.sh)" component new $(HELLO_WIM_CORE) -o $(HELLO_WIM)
	"$$(bash scripts/install-wasm-tools.sh)" component new $(MARKDOWN_PREVIEW_CORE) -o $(MARKDOWN_PREVIEW)
	bash scripts/check-wasm-component.sh $(HELLO_WIM)
	bash scripts/check-wasm-component.sh $(MARKDOWN_PREVIEW)

# Checks the plugins on the host target, where the ABI bindings still compile even though the
# cdylib cannot be linked. `--lib` is what keeps the test run off that link step.
check-plugins:
	cargo fmt --manifest-path $(PLUGINS) --all --check
	cargo clippy --manifest-path $(PLUGINS) --all-targets --locked -- -D warnings
	cargo test --manifest-path $(PLUGINS) --lib --locked

# Runs the native host against the plugins it was built to load. The two variables are what point
# the tests at the components, and they have to be absolute: a test binary runs in its own package
# directory, not here. The tests that need them step aside when they are unset, which is what
# `cargo test --workspace` does on a machine that cannot build the components.
test-plugin-host: build-plugins
	WIM_PLUGIN_WASM="$(CURDIR)/$(HELLO_WIM)" \
		WIM_MARKDOWN_PREVIEW_WASM="$(CURDIR)/$(MARKDOWN_PREVIEW)" \
		cargo test -p wim-plugin-host -p wim --locked
