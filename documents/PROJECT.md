# wim — a Vim-grammar editor in Rust & Wasm

このドキュメントは wim の設計の正 (SSOT) である。元になった議論は bannzai/IdeaMemo#193。

## コンセプト

- Vim 互換ではなく「**Vim の文法を持つ新エディタ**」。Helix のブラウザ版 + Lapce のプラグインモデルという、まだ誰も取っていない位置を狙う
- ポジショニングの核は、モーダル編集コア (Rust) を 1 つの pure crate にし、**headless CLI / デーモン / ブラウザ (Wasm)** の 3 形態で同じコアを動かすこと

## 互換の線引き: 「Vim の文法は残す、Vim の実装は捨てる」

### 残す

- モード、オペレータ + カウント + モーション / テキストオブジェクトの文法 (`d2aw` が読める世界)
- レジスタ、`q` / `@` キーマクロ、マーク、`/` 検索
- Ex コマンドの最小サブセット: `:w` `:q` `:s` `:g` `:norm`。特に `:g/pat/norm @q` は headless ユースケースの核
- **autocmd** はイベントフックとして残す。ただしハンドラは VimScript ではなく、ビルトインコマンド・キー列・Wasm プラグイン関数のいずれかを jsonc 設定で宣言する形にする
- **option** は頻出するものをいくつか実装する。管理は vimrc 形式ではなく VSCode 方式の **jsonc 設定ファイル**

### 捨てる

- VimScript。プラグイン言語は Rust → Wasm に一本化する
- 既存 Vim プラグインの互換
- Vim 独自の正規表現方言 (`\v` 等)。正規表現は Rust の regex crate の文法をそのまま採用し「普通の regex」と宣言する
- 数百個の option の網羅、モードライン

## アーキテクチャ方針

- **編集コア**: ropey (rope) + モーダル状態機械の pure crate。ネイティブにも Wasm にもコンパイルする。本体まで Wasm にする必要はない — ネイティブのマルチプラットフォームは Rust 単体で足りる。Wasm が買うのは (1) ブラウザという追加プラットフォーム (2) プラグイン ABI の統一
- **プラグイン**: Wasm Component Model (wit-bindgen で ABI 定義)。同じ .wasm バイナリがブラウザでもネイティブ (wasmtime) でも動き、サンドボックスが付く。Lapce に先例がある
- **ローカル / リモートファイル編集**: Rust 製の小さなデーモンが FS を WebSocket で提供する (VS Code Remote と同じモデル)。同じデーモンをリモートマシンで動かせばリモート編集がそのまま手に入るため、最初から共通プロトコルで設計する。手軽な補助として File System Access API (Chromium 限定・サーバー不要) も使える
- **ブラウザ描画**: Canvas / WebGL + 自前グリフアトラス。JS ⇔ Wasm 境界は「キー入力を渡してダメージ領域を返す」粒度に粗くする。IME (日本語入力) が最難関のため、隠し textarea を最初から設計に入れる。未確定文字列は DOM オーバーレイを重ねるのではなく、カーソル行を「カーソル前 + 未確定文字列 + カーソル後」の並びで canvas に描き直す (オーバーレイはカーソルより後ろのテキストを隠してしまうため)
- **シンタックスハイライト**: tree-sitter (Wasm 実績あり) を使い、自作しない

## headless Vim は別プロジェクトではなくコアそのもの

「特定のキーマクロをファイルに繰り返し適用する」ツール (sed のキーマクロ版) は、モーダル編集コアの CLI ラッパーでしかない。

```sh
vimacro 'ciwfoo<Esc>j' --repeat-to-eof file.txt
vimacro --global '/^import/' 'A;<Esc>' src/*.ts   # :g/pat/norm 相当
```

- 既存の `nvim --headless +'norm ...'` / `vim -es` は起動が重く複数ファイル適用が書きづらいため、専用ツールの座は空いている
- 「入力ファイル + マクロ → 期待出力」の golden test がそのままコアのテストスイートになる
- IME も描画も無いため、ブラウザ版より圧倒的に早く「日常で使える物」に到達する

## ロードマップ (headless-first)

親 issue は bannzai/wim#12。Phase 1 を最初に完成させ、以降の開発中はずっと実用ツールを手元に持ちながら進める。

### Phase 1: コア crate + headless CLI (vimacro)

| issue | 内容 |
| --- | --- |
| #1 | リポジトリ基盤: Cargo workspace / CI / CLAUDE.md / PROJECT.md |
| #2 | wim-core: バッファ・カーソル・基本モーション |
| #3 | wim-core: モーダル状態機械とキー文法パーサ |
| #4 | golden test 基盤: 入力 + キー列 → 期待出力 |
| #5 | wim-core: 編集オペレータ・レジスタ・undo/redo |
| #6 | wim-core: 検索と Ex コマンド最小サブセット |
| #7 | wim-core: キーマクロ (q/@) とマーク |
| #8 | vimacro CLI: Vim マクロの一括適用ツール |

### Phase 2〜4

| issue | 内容 |
| --- | --- |
| #9 | Phase 2: デーモン + WebSocket プロトコル (ローカル / リモート共通) |
| #10 | Phase 3: ブラウザ UI (Wasm + Canvas + IME)。CI → GitHub Pages のデモ自動デプロイ + ブラウザ E2E を含む |
| #11 | Phase 4: Wasm Component Model プラグイン + Markdown Preview |

ブラウザ拡張 (Markdown Preview 等) は、ターミナル Vim が構造的に持てない能力 (HTML レンダリング・iframe・画像) を開放する最大の差別化ポイントであり、Markdown Preview を first-party のデモにする。

## 命名の経緯

タグラインは **wim — a Vim-grammar editor in Rust & Wasm**。

- 当初案の `rvim` は三重に衝突していたため不採用。`rvim` は Vim 本体に同梱される restricted mode のコマンド (`vim -Z` 相当) として PATH に既に存在し、crates.io の `rvim` も「A text editor in rust」で取得済み、GitHub にも同名リポジトリが複数ある
- 互換を捨てる方針のため、リテラルの "vim" を名前に含めると互換期待を招く。互換を目指した Neovim は vim を残し、捨てた Helix / Kakoune / Lapce は捨てた。その先例に倣い、名前は独自にして Vim らしさはタグラインで伝える
- 衝突チェック (2026-08-31 時点): crates.io の `wim` は空き。GitHub の上位ヒットは Windows Imaging Format (.wim) 関連のみでエディタ領域は無人。検索性は "wim" 単体だと低いため「wim editor」で運用する
- **vimacro** (crates.io 空き) は Phase 1 の headless マクロ適用 CLI のバイナリ名。この CLI は「Vim マクロをファイルに適用するツール」であり、vim 由来の名前が機能説明として正当になる
