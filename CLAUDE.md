# wim

Vim の文法を持つ新エディタ (Vim 互換ではない)。設計の正は `documents/PROJECT.md`。

## 検証方法

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

wasm32 ビルド検査 (`cargo build -p wim-core --target wasm32-unknown-unknown`) は CI が行う。ローカルの Homebrew Rust には wasm32 の std が無いため実行しない。

## 設計原則

- `crates/wim-core` は pure crate。ファイル IO・描画・プラットフォーム依存を入れない。wasm32-unknown-unknown でビルドできる状態を維持する
- 機能を追加したら golden test (`tests/golden/`) を必ず足す
