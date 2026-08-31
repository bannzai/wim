# wim

**wim — a Vim-grammar editor in Rust & Wasm**

Vim 互換ではなく「Vim の文法を持つ新エディタ」です。オペレータ + カウント + モーション、レジスタ、キーマクロ、Ex コマンドの最小サブセットといった Vim の文法は残し、VimScript・Vim 独自の正規表現方言・既存プラグイン互換は捨てます。モーダル編集コアを 1 つの pure crate (`crates/wim-core`) に閉じ込め、headless CLI (`crates/vimacro`) / デーモン / ブラウザ (Wasm) の 3 形態で同じコアを動かします。

- ロードマップ: https://github.com/bannzai/wim/issues/12
- 設計の詳細: [documents/PROJECT.md](documents/PROJECT.md)

## 開発コマンド

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

wasm32 ビルド検査 (`cargo build -p wim-core --target wasm32-unknown-unknown`) は CI が行います。

## License

MIT
