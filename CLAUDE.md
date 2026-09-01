# wim

Vim の文法を持つ新エディタ (Vim 互換ではない)。設計の正は `documents/PROJECT.md`。

## 検証方法

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
make check-plugins
```

`plugins/` は root とは別の Cargo workspace のため `--workspace` に入らない。ABI とサンプルプラグインを触ったら `make check-plugins` (ホスト target の fmt / clippy / unit test) も実行する。component 本体のビルド (`make build-plugins`) は wasm32 の std が要るため CI が行う。

wasm32 ビルド検査 (`cargo build -p wim-core --target wasm32-unknown-unknown`) は CI が行う。ローカルの Homebrew Rust には wasm32 の std が無いため実行しない。`web/` のデモとその Playwright E2E (`make e2e`) も wasm ビルドを前提とするため、同じ理由でローカルでは動かず CI で検証する。

## 設計原則

- `crates/wim-core` は pure crate。ファイル IO・描画・プラットフォーム依存を入れない。wasm32-unknown-unknown でビルドできる状態を維持する
- 機能を追加したら golden test (`crates/wim-core/tests/golden/`) を必ず足す。ケースの書き方は `crates/wim-core/tests/golden/README.md` を参照
