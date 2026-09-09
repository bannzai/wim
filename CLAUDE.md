# wim

設計の正は `documents/PROJECT.md`。

## 検証方法

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

`plugins/` は別 workspace のため、ABI とサンプルプラグインの変更時は `make check-plugins` も実行する。検証内容は `Makefile` を参照。

ローカルの Homebrew Rust には wasm32 の std が無いため、wasm32 ビルド・component ビルド (`make build-plugins`)・Web デモと Playwright E2E (`make e2e`) の検証は CI に委ねる (`.github/workflows/ci.yml`)。

## 設計原則

- `crates/wim-core` は pure crate。ファイル IO・描画・プラットフォーム依存を入れない。wasm32-unknown-unknown でビルドできる状態を維持する
- 機能を追加したら golden test (`crates/wim-core/tests/golden/`) を必ず足す。ケースの書き方は `crates/wim-core/tests/golden/README.md` を参照
