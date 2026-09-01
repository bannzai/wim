# wim プラグイン ABI

`plugin.wit` が wim のプラグイン ABI の正 (SSOT) で、プラグインは Wasm Component Model の
component としてビルドされる。同じ .wasm がブラウザでもネイティブ (wasmtime) でも動くこと、
サンドボックスが標準で付くことが、この形を選んでいる理由 (`documents/PROJECT.md` を参照)。

## world `plugin`

プラグインは `wim:plugin/plugin` world を実装し、3 つの interface を export する。

| interface | 役割 |
| --- | --- |
| `commands` | 名前付きコマンドを公開し、ホストが Ex コマンドとして呼ぶ |
| `events` | ホストが発火したイベント (`buffer-write` 等) を受ける |
| `ui` | HTML 文字列を返し、ホストがパネルとして表示する (Markdown Preview の土台) |

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
変更を表し、ホストは自分がビルドされた時と major.minor が一致しない component を拒否する。
判断の基準は `plugin.wit` 冒頭のコメントに書いてある。

## ビルド

`plugins/` は root の Cargo workspace とは別の workspace になっている。プラグインは
wasm32-wasip2 でしかリンクできないため、root の `cargo test --workspace` /
`cargo clippy --workspace` に混ぜないための分離で、`wit-bindgen` のバージョンは
`plugins/Cargo.toml` の `[workspace.dependencies]` で固定し `plugins/Cargo.lock` を commit する。

```sh
make build-plugins   # wasm32-wasip2 でビルドし、成果物が component であることを確認する
make check-plugins   # ホスト target で fmt / clippy / unit test
```

`wasm32-wasip2` はリンク時に component を直接出力するため、cargo-component や wasm-tools は
要らない (cargo-component は上流で deprecate が進んでいる)。必要なのは rustup の target だけ。

`check-plugins` の test が `--lib` に限定されているのは、cdylib のリンクだけがホスト target で
失敗するため。バインディングの生成とコンパイルはホスト target でも通るので、wit の型検査と
プラグインのロジックの test はローカルで行える。wasm32 系の std を持たない環境 (Homebrew の
Rust など) で `.wasm` を作れないのは Phase 3 と同じで、component のビルドは CI が正になる。

## ホスト

ネイティブ側のホストは `crates/wim-plugin-host` で、バインディングは wit-bindgen ではなく
wasmtime の `bindgen!` が同じ `wit/` から生成する。ロード時に行うことは 3 つある。

- component かどうかを先頭 8 バイトで見る (`scripts/check-wasm-component.sh` と同じ判定)。
  wasm32-wasip2 以外の wasm32 target でビルドしたプラグインは core module になる
- export 名 (`wim:plugin/commands@0.1.0`) が持つ ABI バージョンを見て、major.minor が
  ホストのものと違う component を拒否する。ホスト側のバージョンは `wit/plugin.wit` の
  package 行から読むので、定数として二重に持たない
- linker に何も足さずに instantiate する。world が import する `wim:plugin/buffer` は型だけの
  interface で、wasmtime は関数を持たない instance を linker の定義なしで満たすため、それ以外を
  import する component — WASI を要求する component すべて — はここで弾かれる。これが
  サンドボックスで、実行時に禁止するのではなくロード時に成立しない形にしてある

エディタに組み込まずに動かす入口が `wim plugin run <wasm> <command> [--input TEXT]` で、
標準入力 (または `--input`) のテキストを snapshot として渡し、返ってきた edit を適用した結果を
標準出力へ書く。Ex コマンドへの配線は Phase 4-4 の autocmd・設定と合わせて行う。

```sh
make build-plugins      # component をビルドする (要 wasm32-wasip2)
make test-plugin-host   # ビルドした component をホストで実際にロードして動かす
```

`make test-plugin-host` は component のパスを `WIM_PLUGIN_WASM` でテストに渡す。素の
`cargo test --workspace` ではこの変数が無いため、component を要するテストは skip される
(component をビルドできない環境でも workspace のテストが通るようにするため)。

## 新しいプラグインを足す

`plugins/hello-wim` が最小の実装例で、コマンド 1 つ (`:upcase`)・イベントフック 1 つ・パネル
1 つを持つ。新しいプラグインは同じ形でディレクトリを作り、`plugins/Cargo.toml` の `members`
に足す。
