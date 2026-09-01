# wim

**wim — a Vim-grammar editor in Rust & Wasm**

Vim 互換ではなく「Vim の文法を持つ新エディタ」です。オペレータ + カウント + モーション、レジスタ、キーマクロ、Ex コマンドの最小サブセットといった Vim の文法は残し、VimScript・Vim 独自の正規表現方言・既存プラグイン互換は捨てます。モーダル編集コアを 1 つの pure crate (`crates/wim-core`) に閉じ込め、headless CLI (`crates/vimacro`) / デーモン / ブラウザ (Wasm) の 3 形態で同じコアを動かします。

- ブラウザデモ: https://bannzai.github.io/wim/
- ロードマップ: https://github.com/bannzai/wim/issues/12
- 設計の詳細: [documents/PROJECT.md](documents/PROJECT.md)

## ブラウザデモ

https://bannzai.github.io/wim/ で、`crates/wim-core` をそのまま Wasm にしたエディタが動きます (`crates/wim-wasm` のバインディング + `web/` の Canvas 描画)。`i` `a` `o` で Insert、`Esc` で Normal に戻り、`hjkl` `w` `b` `0` `$` の移動、`x` `dd` `yy` `p` `u` の編集、`:` の Ex コマンドが使えます。シンタックスハイライトはまだ入っていません。

ファイルは 2 通りの方法で開けて、どちらも `:w` で書き戻せます。1 つは手元で `wim serve --root .` を実行し、表示されたアドレスとトークンをページのフォームに入れてデーモン経由で開く方法で、デーモンが `--root` 配下のファイルを読み書きします。もう 1 つは File System Access API 対応ブラウザ (Chrome / Edge) で「ローカルファイルを開く」からファイルを選ぶ方法で、サーバーは要りません。

main への push で GitHub Actions が Wasm をビルドして Pages へデプロイします。手元で動かすには wasm32 ターゲットの Rust と、Cargo.lock と同じバージョンの `wasm-bindgen-cli` が要ります:

```sh
rustup target add wasm32-unknown-unknown
make install-wasm-bindgen
make web  # http://127.0.0.1:4173/
```

## vimacro

`vimacro` は wim のキー列と Ex コマンドをファイルへ一括適用する headless CLI です。sed がスクリプトを適用するのと同じ位置づけで、Vim のマクロをそのまま書けます。

### インストール

```sh
cargo install --git https://github.com/bannzai/wim vimacro
```

### 使用例

各行の最初の単語を `foo` に書き換える。`--repeat-to-eof` は各行の行頭にカーソルを置いてキー列を実行するため、マクロ側に `j` は要りません:

```sh
vimacro 'ciwfoo<Esc>' --repeat-to-eof notes.txt
```

`import` で始まる行の末尾に `;` を足す (`:g/^import/norm A;<Esc>` 相当):

```sh
vimacro --global '^import' 'A;<Esc>' src/app.ts
```

Ex コマンドを直接実行する:

```sh
vimacro --ex '%s/foo/bar/g' notes.txt
```

ファイル引数を省くと標準入力を読み、標準出力へ書くのでパイプに繋げます:

```sh
cat notes.txt | vimacro 'A!<Esc>'
```

複数ファイルは `-i` (`--in-place`) で書き換えます (`#` で始まる行を削除する例):

```sh
vimacro -i --ex 'g/^#/d' a.md b.md
```

Ex コマンドとキー列は併用でき、Ex コマンドが先に実行されます。`--ex` を使う時は最初の引数もファイル名として読むため、キー列は `--keys` で明示します:

```sh
vimacro --ex '%s/foo/bar/g' --keys 'ggA!<Esc>' notes.txt
```

### 動作

- キー記法 (`<Esc>` `<CR>` `<BS>` `<Tab>` `<lt>` `<C-x>`) は [crates/wim-core/tests/golden/README.md](crates/wim-core/tests/golden/README.md) を参照してください
- 既定の出力は標準出力です。ファイルへ書き戻すのは `-i` を付けた時だけで、`-i` なしで複数ファイルを渡すと結果が繋がってしまうためエラーになります
- 終了コードは、全ファイルが最後まで実行できたら 0、キー列のパースエラー・ファイルの読み書き失敗・コアが弾いたキーがあれば非 0 です。エラーはファイル名付きで標準エラーへ出ます
- CRLF のファイルは LF として読み、CRLF のまま書き戻します
- オプションの完全な説明は `vimacro --help` にあります

## 開発コマンド

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

wasm32 ビルド検査 (`cargo build -p wim-core --target wasm32-unknown-unknown`) と、デモページに対する Playwright の E2E (`make e2e`) は CI が行います。

## License

MIT
