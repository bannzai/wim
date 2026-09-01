# wim

**wim — a Vim-grammar editor in Rust & Wasm**

Vim 互換ではなく「Vim の文法を持つ新エディタ」です。オペレータ + カウント + モーション、レジスタ、キーマクロ、Ex コマンドの最小サブセットといった Vim の文法は残し、VimScript・Vim 独自の正規表現方言・既存プラグイン互換は捨てます。モーダル編集コアを 1 つの pure crate (`crates/wim-core`) に閉じ込め、headless CLI (`crates/vimacro`) / デーモン / ブラウザ (Wasm) の 3 形態で同じコアを動かします。

- ブラウザデモ: https://bannzai.github.io/wim/
- ロードマップ: https://github.com/bannzai/wim/issues/12
- 設計の詳細: [documents/PROJECT.md](documents/PROJECT.md)

## ブラウザデモ

https://bannzai.github.io/wim/ で、`crates/wim-core` をそのまま Wasm にしたエディタが動きます (`crates/wim-wasm` のバインディング + `web/` の Canvas 描画)。`i` `a` `o` で Insert、`Esc` で Normal に戻り、`hjkl` `w` `b` `0` `$` の移動、`x` `dd` `yy` `p` `u` の編集、`:` の Ex コマンドが使えます。

シンタックスハイライトは tree-sitter で、拡張子が `.rs` / `.md` のバッファに当たります。ページ上の「シンタックスハイライトの例」ボタンで、ファイルを開かずに色付きのバッファを読み込めます。ハイライトは `wim-wasm` / `web` 層の担当で、`crates/wim-core` は言語を一切知りません。tree-sitter のランタイムと文法 (`.wasm` と `highlights.scm`) は `scripts/vendor-tree-sitter.sh` が `web/vendor/tree-sitter/` へ取得したものをそのままコミットしてあり、Pages のデプロイも E2E もこのファイルを読みます。バージョンを上げるときだけ `make vendor-tree-sitter` を実行します。対応言語を足すには、同スクリプトの `GRAMMARS` と `web/highlight.js` の `LANGUAGES` にそれぞれ 1 行足します。

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

## プラグイン

プラグインは Wasm Component Model の component で、ABI は [wit/plugin.wit](wit/plugin.wit) が定義します。コマンド登録・イベントフック・HTML パネルの 3 つを export し、バッファはハンドルではなくテキストの値で受け渡す純関数型のインターフェースです。ABI の読み方とビルド方法は [wit/README.md](wit/README.md)、最小の実装例は [plugins/hello-wim](plugins/hello-wim) にあります。

```sh
rustup target add wasm32-unknown-unknown
make build-plugins
```

ビルドした component は `crates/wim-plugin-host` が wasmtime でロードします。プラグインには WASI を一切渡さないため、ファイル IO・ネットワーク・時刻に触れる component はロード時点で拒否されます。エディタに組み込まずに動かすには `wim plugin run` を使います (ネイティブエディタの Ex コマンドへの配線は Phase 4-4)。

```sh
echo 'hello wim' | cargo run -p wim -- plugin run plugins/target/wasm32-unknown-unknown/release/hello_wim.component.wasm upcase
# => HELLO WIM
```

### ブラウザで動かす

同じ component をブラウザデモでも動かせます。[jco](https://github.com/bytecodealliance/jco) が component を ES module へ transpile し、デモがそれを import してプラグインのコマンドを `:upcase` のような Ex コマンドとして実行します。transpile 生成物は `.wasm` から機械生成されるためコミットせず、`make build-web-plugins` が毎回作り直します (`web/plugins/` は gitignore)。jco のバージョンは `web/package.json` で固定します。

```sh
make build-web-plugins
make web  # http://127.0.0.1:4173/ で :upcase が使えます
```

ブラウザ側のサンドボックスとバージョン検査はネイティブと同じ形です。world が import する `wim:plugin/buffer` は型だけの interface なので jco の出力は何も import せず、WASI シムも入りません (`scripts/transpile-plugins.sh` が生成物を検査します)。ロード時には export 名が持つ ABI バージョン (`wim:plugin/commands@0.1.0`) を `wit/plugin.wit` の package バージョンと突き合わせ、完全に一致しない module を拒否します。

「同一の .wasm がネイティブとブラウザの両方で動く」ことは E2E が機械的に確かめます。`web/e2e/plugin.spec.js` が同じ component をネイティブホスト (`wim plugin run`) とブラウザの両方に同じ入力で通し、返ってきたバッファとエラーメッセージが一致することを検証します。

## 設定 (wim.jsonc) と autocmd

設定ファイルは vimrc ではなく VS Code 方式の JSONC で、`autocmds` にイベントとハンドラの組を宣言します。ハンドラは VimScript ではなく、ビルトインの Ex コマンド・キー列・Wasm プラグイン関数の 3 つから選びます。形式の全体とイベント一覧は [documents/CONFIG.md](documents/CONFIG.md) にあります。

```jsonc
{
  "autocmds": [
    // 保存の直前に行末の空白を落とす (パターンは Vim の方言ではなく Rust の regex)
    { "event": "buffer-write", "handler": { "kind": "ex", "command": "%s/\\s+$//" } },
    // 保存の直前にプラグインへ知らせる
    { "event": "buffer-write", "handler": { "kind": "plugin", "plugin": "hello-wim" } },
  ],
}
```

イベントを報告するのはコアで、購読と配線はホストの担当です (`crates/wim-core` は設定ファイルもプラグインも知りません)。ネイティブでは `wim edit` が設定を読み、キー列をファイルに適用しながら autocmd を実行します。`:w` はその場でファイルを書き、`buffer-write` は書き込みの直前に発火するため、上の例では空白を落としたバッファが書き込まれます。

```sh
wim edit notes.txt --keys ':w<CR>' --config web/wim.jsonc \
  --plugin hello-wim=plugins/target/wasm32-unknown-unknown/release/hello_wim.component.wasm
# => buffer-write ex: %s/\s+$//
#    buffer-write plugin hello-wim: hello-wim saw `buffer-write` on notes.txt
```

ブラウザデモは同じ形式の `web/wim.jsonc` を fetch して同じイベントで同じハンドラを実行します。「同じ設定が両方のホストで発火する」ことは E2E が確かめます (`crates/wim/tests/autocmd.rs` と `web/e2e/autocmd.spec.js`)。

## 開発コマンド

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
make check-plugins  # plugins/ は別 workspace のため --workspace に入りません
```

wasm32 ビルド検査 (`cargo build -p wim-core --target wasm32-unknown-unknown`)、プラグインの component ビルド (`make build-plugins`)、ビルドした component を実際にロードするホストのテスト (`make test-plugin-host`)、その component を transpile したブラウザホストを含むデモページの Playwright E2E (`make e2e`) は CI が行います。

## License

MIT
