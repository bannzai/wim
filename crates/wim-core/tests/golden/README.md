# golden test

「この文が入っていて、このキーを打つと、こうなる」を 1 ファイル 1 ケースで書くテスト。ランナーは `crates/wim-core/tests/golden.rs`、ケースは `cases/*.toml`。`cargo test --workspace` に含まれる。

ケースを 1 ファイル足せばテストが 1 件増える。Rust のコードを書く必要はない。

## ケースの形式

```toml
name = "w jumps to the start of the next word"
input = "foo bar baz"
keys = "w"
expected = "foo bar baz"
expected_cursor = [0, 4]
```

| フィールド | 必須 | 内容 |
| --- | --- | --- |
| `name` | 任意 | ケースの説明。失敗時の表示にだけ使う |
| `input` | 必須 | キーを打つ前のバッファ |
| `keys` | 必須 | 打つキー列 |
| `expected` | 必須 | キーを打った後のバッファ |
| `expected_cursor` | 任意 | 打った後のカーソル `[line, col]`。省略するとカーソルを検証しない |

ここにない名前のフィールドを書くとパースエラーになり、そのケースはファイル名付きで fail する。

複数行のテキストは TOML の複数行リテラル文字列で書く。開き `'''` の直後の改行は TOML が落とすので、1 行目は次の行から書き始める:

```toml
input = '''
alpha
bravo'''
```

末尾に改行があるバッファは、閉じ `'''` を次の行に置く (`"alpha\n"` になる):

```toml
input = '''
alpha
'''
```

## 規約

- **カーソルの初期位置は常に (0, 0)**、モードは常に Normal。別の位置から始めたいケースは、`keys` の先頭にそこまでのモーションを含める (例: `keys = "wibar <Esc>"`)
- キー記法は `wim_core::parse_keys` が読むもの: `<Esc>` `<CR>` `<BS>` `<Tab>` `<lt>` (リテラルの `<`)、`<C-x>` (Ctrl 併用)。それ以外の文字はその文字自身を表す
- `:norm {keys}` に渡すキー列は、コマンドラインへ打ち込む「文字」として書く。`<Esc>` とそのまま書くと `parse_keys` がコマンドライン入力の取り消しキーに変えてしまうので、`<lt>Esc>` と書いて `<` `E` `s` `c` `>` の 5 文字を打ち込む (`:norm` 側が `parse_keys` でそれを `<Esc>` として読む)。例: `keys = ":g/^import/norm A;<lt>Esc><CR>"`
- ファイル名はケースの内容が分かる kebab-case にする (`motion-word-forward.toml`、`insert-open-line-below.toml`)。ランナーはファイル名順に実行する
- 検証するのはバッファのテキストと (指定した場合の) カーソル位置だけ。モード・レジスタ等はコア側のユニットテストで見る
- 改行は LF で書く。Windows のチェックアウトで CRLF になった場合はランナーが LF に正規化してから比較する

## 追加の手順

1. `cases/` に `<内容>.toml` を作る
2. `cargo test --workspace` を実行する
3. 落ちたら、失敗表示に出る expected / actual の行単位 diff を見て、ケースの誤りかコアの不具合かを判断する

失敗時は「ファイル名 — name / keys / text の diff / cursor の期待値と実測値」が並ぶ。1 つの `#[test]` で全ケースを回すので、複数落ちていても 1 回の実行ですべて出る。

## いま入っていないもの

マクロ記録・再生 (`q` `@`) は未実装 (issue #7)。そのケースは実装時にここへ追加する。

レジスタの中身・モード・undo 履歴の深さのように、バッファのテキストとカーソル位置に現れない状態はこの形式では検証できない。それらはコア側のユニットテスト (`src/editor.rs` 等) で見る。`:w` `:q` が返す `Effect` も同様で、`src/editor.rs` のユニットテストで見る。
