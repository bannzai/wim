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
1 つに揃える。1.0.0 に達するまでは minor が破壊的変更、patch が後方互換な追加を表し、ホストは
自分がビルドされた時と major.minor が一致しない component を拒否する。判断の基準は
`plugin.wit` 冒頭のコメントに書いてある。

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

## 新しいプラグインを足す

`plugins/hello-wim` が最小の実装例で、コマンド 1 つ (`:upcase`)・イベントフック 1 つ・パネル
1 つを持つ。新しいプラグインは同じ形でディレクトリを作り、`plugins/Cargo.toml` の `members`
に足す。
