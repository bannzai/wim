# wim

設計の正は `documents/PROJECT.md`。

## 検証方法

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

`plugins/` は別 workspace のため、ABI とサンプルプラグインの変更時は `make check-plugins` も実行する。検証内容は `Makefile` を参照。

引数なしの `make` は `web` target を実行し、Wasm とプラグインをビルドしてブラウザデモを http://127.0.0.1:4173/ で起動する (人が手で動作確認するための入口。検査・テストは含めず CI が行う)。`make verify` は上の 3 コマンドと `make check-plugins`、`bash scripts/vendor-tree-sitter.sh --check` をまとめて走らせる。

ローカルの Homebrew Rust には wasm32 の std が無いため、wasm32 ビルド・component ビルド (`make build-plugins`)の検証は CI に委ねる (`.github/workflows/ci.yml`)。Web デモと Playwright E2E (`make e2e`) は PR ごとには動かさず、必要な時に `gh workflow run e2e.yml --ref <ブランチ>` で起動し、dispatch の run は PR の checks に出ないため、`gh run list --workflow e2e.yml --branch <ブランチ> --limit 1 --json databaseId --jq '.[0].databaseId'` で得た run ID を `gh run watch <run ID> --exit-status` に渡して成否を確かめる (`.github/workflows/e2e.yml`。E2E を PR の必須チェックにしない方針は bannzai/castle の setup-ci skill)。

## 設計原則

- `crates/wim-core` は pure crate。ファイル IO・描画・プラットフォーム依存を入れない。wasm32-unknown-unknown でビルドできる状態を維持する
- 機能を追加したら golden test (`crates/wim-core/tests/golden/`) を必ず足す。ケースの書き方は `crates/wim-core/tests/golden/README.md` を参照
