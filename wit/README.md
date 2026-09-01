# wim プラグイン ABI

`plugin.wit` が wim のプラグイン ABI の正 (SSOT) で、プラグインは Wasm Component Model の
component としてビルドされる。同じ .wasm がブラウザでもネイティブ (wasmtime) でも動くこと、
サンドボックスが標準で付くことが、この形を選んでいる理由 (`documents/PROJECT.md` を参照)。

## world `plugin`

プラグインは `wim:plugin/plugin` world を実装し、3 つの interface を export する。

| interface | 役割 |
| --- | --- |
| `commands` | 名前付きコマンドを公開し、ホストが Ex コマンドとして呼ぶ |
| `events` | ホストが発火したイベントを受ける。名前は `documents/CONFIG.md` の一覧 (`buffer-write` 等) |
| `ui` | HTML 文字列を返し、ホストがパネルとして表示する (Markdown Preview の土台) |

`events` に配送されるのは、プラグインが `subscriptions` で挙げたイベントだけ。どのイベントを
どのハンドラに束ねるかは wim.jsonc の autocmd がホスト側で決めるため、プラグインが購読して
いないイベントに束ねた autocmd は、発火しない設定としてホストが弾く (`documents/CONFIG.md`)。

型だけを持つ 4 つ目の interface `buffer` が、この 3 つが受け渡すバッファのスナップショットと
編集結果を定義する。ホストの状態を指すハンドルは渡さず、呼び出しごとにテキストを値で渡して
編集結果を値で受け取る純関数型にしてあるため、プラグインはホストのバッファを直接触れない。

`buffer` は world に export されず、encode 後の component には `import wim:plugin/buffer@0.1.0`
として現れる。関数を持たない型だけの instance のため、ホストは何も実装せずに instantiate できる
(`wasi:io/error` と同じ形)。

## バージョニング

ABI のバージョンは wit の package バージョン (`wim:plugin@0.1.0`) そのもので、`wit/` 配下で
1 つに揃える。1.0.0 に達するまでは minor が ABI の変更 (export への関数追加を含む — 新しい
ホストは古い component にその export が無いと instantiate できない)、patch が ABI に触れない
変更を表す。判断の基準は `plugin.wit` 冒頭のコメントに書いてある。

ホストが受け入れるのは、自分がビルドされた時とバージョンが完全に一致する component だけで、
patch 差も許容しない。export 名にはフルバージョンが入る (`wim:plugin/commands@0.1.0`) ため、
ホストの bindings が探す export 名も `@0.1.0` で固定されており、patch だけ違う component は
バージョン検査を通しても instantiate で export が見つからずに失敗するだけになる。patch は ABI
に触れない変更にしか使わないので、patch を上げた時にホストとプラグイン双方の再ビルドが要る
こと自体に実害はない。

## ビルド

`plugins/` は root の Cargo workspace とは別の workspace になっている。プラグインの cdylib は
wasm でしかリンクできないため、root の `cargo test --workspace` /
`cargo clippy --workspace` に混ぜないための分離で、`wit-bindgen` のバージョンは
`plugins/Cargo.toml` の `[workspace.dependencies]` で固定し `plugins/Cargo.lock` を commit する。

```sh
make build-plugins   # wasm32-unknown-unknown でビルドして componentize する
make check-plugins   # ホスト target で fmt / clippy / unit test
```

target は `wasm32-wasip2` ではなく `wasm32-unknown-unknown` を使う。wasip2 の std は
wasi:io などの WASI import を component に残し、ホストのサンドボックス (WASI を import する
component をロード時に拒否する) が自分のプラグインを弾いてしまうため。unknown-unknown の
core module には ABI 由来の import しか残らず、pin した wasm-tools
(`scripts/install-wasm-tools.sh` がリリースバイナリを取得) の `component new` で component に
する (cargo-component は上流で deprecate が進んでいるため使わない)。

`check-plugins` の test が `--lib` に限定されているのは、cdylib のリンクだけがホスト target で
失敗するため。バインディングの生成とコンパイルはホスト target でも通るので、wit の型検査と
プラグインのロジックの test はローカルで行える。wasm32 系の std を持たない環境 (Homebrew の
Rust など) で `.wasm` を作れないのは Phase 3 と同じで、component のビルドは CI が正になる。

## ホスト

ネイティブ側のホストは `crates/wim-plugin-host` で、バインディングは wit-bindgen ではなく
wasmtime の `bindgen!` が同じ `wit/` から生成する。ロード時に行うことは 3 つある。

- component かどうかを先頭 8 バイトで見る (`scripts/check-wasm-component.sh` と同じ判定)。
  componentize していないビルド成果物 (素の core module) はここで弾かれる
- export 名 (`wim:plugin/commands@0.1.0`) が持つ ABI バージョンを見て、ホストのものと完全に
  一致しない component を拒否する。ホスト側のバージョンは `wit/plugin.wit` の package 行から
  読むので、定数として二重に持たない
- linker に何も足さずに instantiate する。world が import する `wim:plugin/buffer` は型だけの
  interface で、wasmtime は関数を持たない instance を linker の定義なしで満たすため、それ以外を
  import する component — WASI を要求する component すべて — はここで弾かれる。これが
  サンドボックスで、実行時に禁止するのではなくロード時に成立しない形にしてある

linker が断つのは「できること」で、プラグインがなお使えるのは時間とメモリになる。呼び出しは
すべてホストのスレッドで同期実行されるため、store 側で両方に上限を置く。1 回の呼び出しごとに
fuel (wasm 命令数相当) を与え直し、guest の linear memory の上限も設ける。無限ループや際限の
ない `memory.grow` は上限に当たって trap し、ホストにはエラーとして返る。上限値と選定根拠は
`crates/wim-plugin-host/src/lib.rs` の `CALL_FUEL` / `MEMORY_LIMIT` に書いてある。

エディタに組み込まずに動かす入口が 2 つある。`wim plugin run <wasm> <command> [--input TEXT]`
は標準入力 (または `--input`) のテキストを snapshot として渡し、返ってきた edit を適用した
結果を標準出力へ書く。`wim plugin render <wasm> [--input TEXT] [--name NAME]` は同じ snapshot を
`ui.render` に渡し、返ったパネルの HTML を標準出力へ、見出しを標準エラーへ書く。`--name` が
要るのは、どのバッファにパネルを出すかをプラグインがバッファ名で決められるため
(`markdown-preview` は `.md` / `.markdown` だけに出す)。パネルを持たないという答え (`none`) は
失敗ではないので、標準出力には何も書かずに終了ステータス 0 で終わる。

```sh
make build-plugins      # component をビルドする (要 wasm32-unknown-unknown)
make test-plugin-host   # ビルドした component をホストで実際にロードして動かす
```

`make test-plugin-host` は component のパスを `WIM_PLUGIN_WASM` でテストに渡す。素の
`cargo test --workspace` ではこの変数が無いため、component を要するテストは skip される
(component をビルドできない環境でも workspace のテストが通るようにするため)。

## ブラウザホスト

ブラウザ側のホストは `web/plugins.js` で、wasmtime の代わりに
[jco](https://github.com/bytecodealliance/jco) が同じ component を ES module へ transpile した
ものをロードする。`scripts/transpile-plugins.sh` が transpile とマニフェストの生成を行い、
`make build-web-plugins` がビルドの一部として呼ぶ。生成物 (`web/plugins/`) はコミットしない:
ソースは同じリポジトリの `plugins/` にあり、`.wasm` から機械的に再生成できるため、ソースが
リポジトリに無い `web/vendor/` の tree-sitter とは扱いが違う。

ロード時に行うことはネイティブと同じ 3 つで、どこで判定するかだけが違う。

- component かどうかは transpile の入力を `scripts/check-wasm-component.sh` が先頭 8 バイトで
  見る。ブラウザに届くのは transpile 済みの JS なので、この判定はビルド時に済んでいる
- export 名が持つ ABI バージョンを見て、ホストのものと完全に一致しない module を拒否する。
  jco は `wim:plugin/commands@0.1.0` をそのまま ES module の export 名にするため、ネイティブが
  component の export 名を読むのと同じものを `Object.keys` で読むことになる。ホスト側の
  バージョンは transpile 時に `wit/plugin.wit` の package 行から読んでマニフェストへ書く
- 何も import させない。world が import する `wim:plugin/buffer` は型だけの interface で、jco は
  それに対して import を一切生成しない。生成された JS に `import` 文が無いことを
  `scripts/transpile-plugins.sh` が検査するので、WASI シムが混ざれば transpile の時点で失敗する

fuel とメモリ上限に相当するものはブラウザ側には無い。ネイティブの上限はホストのスレッドを
止めないためのもので、ブラウザではページのタブがその役割を負う。

エディタへの配線はデモ (`web/main.js`) にある。ロード時に `list-commands` で公開コマンドを
登録し、コアが知らない `:<name>` は unknown-ex-command の effect としてホストへ渡ってくるので、
ホストがその名前でプラグインのコマンドを解決して実行する。プラグインが返した edit はホストが
適用する。どのプラグインにも無い名前はホストが `not an editor command` として拒否する。

```sh
make build-web-plugins  # component をビルドして transpile する (要 wasm32-unknown-unknown)
make e2e                # 同じ component をネイティブとブラウザの両方に通して突き合わせる
```

`web/e2e/plugin.spec.js` が「同一の .wasm がネイティブとブラウザの両方で動く」ことの機械検証で、
同じ入力を `wim plugin run` とデモの両方に通し、返るバッファとエラーメッセージが一致することを
確かめる。component のパスは `make test-plugin-host` と同じ `WIM_PLUGIN_WASM` で渡す。

## パネルの HTML をどう描くか

`ui.render` が返す HTML は、ホストが信頼しないものとして扱う。サニタイズ (タグの除去) では
なく隔離で扱うのが wim の方針で、理由は Markdown Preview そのものにある。CommonMark は生の
HTML をそのまま通すと定めているため、`<script>` を書いた `.md` を素直に描くプラグインの出力に
はスクリプトが混ざる。ここでタグを剥がすと「Markdown を Markdown として描く」ことをやめる
ことになり、剥がし漏れが 1 つでもあればそのまま実行されてしまう。隔離は逆で、何が入っていても
実行できる場所に置かないという 1 つの判断で閉じる。

ブラウザ側 (`web/main.js`) は、パネルごとに `sandbox` 属性を空文字列で付けた iframe を作り、
HTML を `srcdoc` に入れる。空の `sandbox` はブラウザの制限をすべて有効にした状態で、
`allow-scripts` を与えていないため中のスクリプトは実行されず、`allow-same-origin` も無いため
不透明なオリジンに置かれてページの DOM・Cookie・ストレージには触れられない。加えて、ホストが
被せる文書側で `default-src 'none'` の CSP を宣言し、画像 (`img-src`) 以外のサブリソース取得を
止める。DOMPurify のようなサニタイザを足さないのは、隔離で閉じている問題に依存を 1 つ増やす
ことになるため。

ネイティブ側でパネルを描く GUI はまだ無く、入口は `wim plugin render` (下記) だけになる。
描く側が増えた時も、同じ「実行できない場所に置く」を満たすこと。

## エディタへの配線

デモがパネルを描き直すのは、コアが `text-changed` を報告した時と、バッファ自体が入れ替わった
時 (ファイルを開いた・サンプルを読み込んだ・プラグインがバッファを書き換えた) で、これは
`render` を「パネルを開く時とバッファが変わって描き直す時に呼ぶ」と定めた wit の記述どおりの
タイミングになる。wim.jsonc の autocmd には束ねない: autocmd のハンドラが呼ぶのは `on-event`
で、返るのは `edit` であってパネルではないため、パネルの更新は autocmd の仕事ではない。
`markdown-preview` が `subscriptions` で何も購読していないのはそのためで、これを autocmd に
束ねた設定は「配送されない autocmd」としてホストが弾く。

## 新しいプラグインを足す

`plugins/hello-wim` が最小の実装例で、コマンド 1 つ (`:upcase`)・イベントフック 1 つ・パネル
1 つを持つ。`plugins/markdown-preview` は `ui` だけを実装した実用例で、`.md` / `.markdown` の
バッファを HTML にして返し、それ以外のバッファでは `none` を返してパネルを閉じさせる
(Markdown → HTML は pulldown-cmark)。新しいプラグインは同じ形でディレクトリを作り、
`plugins/Cargo.toml` の `members`・`Makefile` の `build-plugins` / `build-web-plugins` に足す。
