# wim.jsonc — 設定ファイルと autocmd

wim の設定ファイル形式の正 (SSOT)。設計方針の出所は [PROJECT.md](PROJECT.md) の「autocmd はイベントフックとして残す。ただしハンドラは VimScript ではなく、ビルトインコマンド・キー列・Wasm プラグイン関数のいずれかを jsonc 設定で宣言する形にする」。

## 方言

VS Code と同じ **JSONC**、つまり JSON に次の 2 つだけを足したもの:

- 行コメント (`// …`) とブロックコメント (`/* … */`)
- 配列・オブジェクトの末尾カンマ

これ以外の拡張 (シングルクォート文字列・16 進数・カンマ省略・引用符なしのキー) はどちらのホストも受け付けない。ネイティブは `jsonc-parser` crate をこの 2 つに絞って呼び、ブラウザは `web/config.js` がコメントと末尾カンマだけを落として `JSON.parse` に渡す。片方のホストだけが読める設定を作らないための線引きなので、どちらかを緩める時は両方を同時に変える。

## スキーマ

```jsonc
{
  "autocmds": [
    { "event": "<イベント名>", "handler": { "kind": "ex", "command": "%s/\\s+$//" } },
    { "event": "<イベント名>", "handler": { "kind": "keys", "keys": "ggVGd" } },
    { "event": "<イベント名>", "handler": { "kind": "plugin", "plugin": "hello-wim" } },
  ],
}
```

- トップレベルは `autocmds` だけ。書いていない項目・知らない項目はエラーにする (黙って無視しない)
- `autocmds` は宣言順に実行される配列。同じイベントに何個でも束ねられる
- `event` は下記のイベント名のいずれか。それ以外は「そのイベントは無い」とエラーになる。発火しない autocmd を黙って抱えないため、設定を読んだ時点で弾く
- `handler` は `kind` で分岐する。`kind` ごとに必要な項目は 1 つで、余分な項目はエラーになる

## イベント

名前は Vim の autocmd に倣い、プラグイン ABI (`wit/plugin.wit`) のイベント名の綴りに合わせた kebab-case。実体は `crates/wim-core/src/effect.rs` の `Event`。

| イベント | 発火点 | 誰が発火するか |
| --- | --- | --- |
| `buffer-write` | バッファを書き出す直前。Vim の `BufWrite` (= `BufWritePre`) | コア。`:w` の `SaveRequested` の**直前**に並べて返す |
| `buffer-write-post` | 書き出しが成功した直後。Vim の `BufWritePost` | ホスト。書けたかどうかを知っているのはホストなのでコアは発火しない |
| `text-changed` | 1 つの変更がテキストを変えた時。Vim の `TextChanged` | コア |
| `mode-changed` | キーがモードを変えた時。Vim の `ModeChanged` | コア |

- `buffer-write` が `SaveRequested` より前に返るので、ホストが順に処理すれば「ハンドラが編集し終えたバッファ」が書き込まれる。行末の空白を落とすような autocmd はこれで成立する
- `text-changed` は「閉じた変更 1 つ」につき 1 回。Insert セッションは `<Esc>` で閉じた時に 1 回で、1 文字ごとには出ない (Vim の `TextChanged` と同じ)。undo / redo も 1 回ずつ出る
- イベントは「ユーザーが打ったキー」に紐づく。マクロ (`@q`) や `.` が内部で打つキーは、それを打った 1 キーのイベントにまとまる
- `mode-changed` だけがペイロードを持ち、`{"from":"NORMAL","to":"INSERT"}` という JSON 文字列でプラグインに渡る。他のイベントのペイロードは空文字列

## ハンドラ

| `kind` | 項目 | 実行内容 |
| --- | --- | --- |
| `ex` | `command` | Ex コマンドを 1 行実行する。先頭の `:` は書かない |
| `keys` | `keys` | キー列を打つ。記法は `wim_core::parse_keys` のもの (`<Esc>` `<CR>` `<BS>` `<Tab>` `<lt>` `<C-x>`) |
| `plugin` | `plugin` | ホストがその名前で読み込んだプラグインの `on-event` を呼び、返ってきた edit を適用する |

書く時の注意:

- `:s` や `:g` のパターンは Vim の方言ではなく **Rust の regex** (PROJECT.md)。行末の空白は `\s\+$` ではなく `\s+$`
- `keys` は素のキー記法で書く。`:norm` の引数のように `<lt>Esc>` と書く必要はない
- ハンドラが半端に開いたコマンド (Insert モード等) は、実行後に `<Esc>` 2 回で閉じられる。`:norm` がキー列の最後に行うのと同じ

## 実行の規則

- **入れ子にしない**: ハンドラの実行中に発火したイベントでは、ハンドラを実行しない。`text-changed` のハンドラが編集すると再び `text-changed` になり、止まらなくなるため。Vim も `nested` を書かない限り入れ子にしない
- **失敗しても続ける**: あるハンドラが失敗しても、同じイベントに束ねた残りのハンドラは実行する。失敗はハンドラごとの報告行に出る (`wim edit` は標準出力の行と終了ステータス、デモは autocmd の行)
- **プラグインは購読しているイベントだけ**受け取る (`wit/plugin.wit`)。購読していないイベントに束ねた `plugin` ハンドラは、発火しない設定としてホストが弾く

## ホストごとの読み方

| ホスト | 設定 | プラグインの解決 |
| --- | --- | --- |
| `wim edit --config wim.jsonc FILE --keys …` | `--config` で渡したファイル | `--plugin <名前>=<component.wasm>` (複数可) |
| ブラウザデモ (`web/`) | ページと同じ場所に置いた `wim.jsonc` | `make build-web-plugins` が transpile した `web/plugins/manifest.json` の名前 |

設定は「どのイベントで何を走らせるか」だけを持ち、プラグインの実体の場所は持たない。同じ `wim.jsonc` を両方のホストが読めるようにするため、名前からファイルへの解決はホストの引数・マニフェストの側に置いてある。

## 例

`web/wim.jsonc` がデモの設定で、そのままネイティブでも動く:

```sh
wim edit notes.txt --keys ':w<CR>' --config web/wim.jsonc \
  --plugin hello-wim=plugins/target/wasm32-unknown-unknown/release/hello_wim.component.wasm
```
