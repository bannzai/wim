# 0003. デーモンのパス閉じ込めを、ルートのディレクトリハンドル起点の beneath セマンティクスにする

## Status

Accepted

## Context

PR #29 への Codex レビュー指摘を issue #31 に繰り越したもの。

`Root::resolve` はリクエストのパスを `fs::canonicalize` で解決し、`starts_with(root)` でルート配下であることを確かめてから、返した絶対パスを後続の `read_dir` / `read_to_string` / `OpenOptions::open` / `rename` に渡していた。検査するパスと使うパスが別の system call に分かれているため、その間にパス上のコンポーネントを symlink へ差し替えられると、検査を通した後に置き換わった実体 (ルート外) を開くことになる。典型的な check/use race (TOCTOU) で、検査を重ねても閉じない。

脅威モデルは ADR 0001 のとおり、デーモンは loopback + トークンで守られ、認証済みクライアントは信頼する。ここで問題になるのは、**ルート配下にファイルを作れる別のローカルプロセスが、認証済みクライアントの正当なリクエストをルート外へ誘導する**ケースである。クライアントは自分が `notes.md` を読み書きしているつもりで、実際にはルート外のファイルを読まされる・上書きさせられる。Phase 2 のローカル利用では影響が限定的だが、Phase 2 本来の狙いであるリモート常駐では、デーモンのルートがそのマシンで守るべき境界そのものになるため解消しておきたい。

ADR 0002 の 4 で `fs.watch` は字句パスで監視する (canonical 化しない) と決めており、本 ADR が置き換えるのは read / write / list のデータアクセス経路だけである。

## Decision

### cap-std を導入し、ルートのディレクトリハンドル起点の相対 open にする

`Root` はルートを canonicalize したうえで `cap_std::fs::Dir::open_ambient_dir` でディレクトリハンドルを保持する。`ambient_authority()` を使うのは起動時のこの 1 回だけで、以後リクエストが名指すパスはすべてこのハンドルからの相対 open で扱う。

リクエストのパスは **字句的にだけ** 相対パスへ解決する (`Root::relative`)。相対パスはルートに join し、絶対パスはそのまま扱い、どちらも `.` / `..` を字句的に畳んだうえでルートの字句 prefix を strip する。prefix でなければ従来と同じ `permission_denied` (「outside the directory this daemon serves」) で拒否する。ファイルシステムには何も問い合わせない。

パス上に symlink があるかどうかは cap-std が open の各コンポーネント解決時に検査する。ルート外へ出る symlink は open そのものが失敗するため、「検査してから使う」の間が消える。検査と使用が 1 つの操作に統合されるので、検査後の差し替えが効かない構造になる。

各メソッドの対応:

- `fs.read`: `dir.open` + `read_to_string`
- `fs.list`: `dir.read_dir`
- `fs.write`: staging は `dir.open_with` (`create_new`) → write → `dir.rename`。ADR 0002 の 6 の in-place フォールバックは `dir.open_with` (`write` + `truncate` + `follow(FollowSymlinks::No)`)。パーミッション引き継ぎも `dir.metadata` + `File::set_permissions` でハンドル経由にする
- `fs.watch`: 触らない。ADR 0002 の 4 のとおり `Root::resolve_lexically` による字句解決のままで、notify に渡す絶対パスの組み立ても不変。監視は読み書きを伴わないため beneath open の対象にならない

cap-std の API は同期のため、ハンドル経由の操作は `tokio::task::spawn_blocking` で包む (`tokio::fs` が内部でしているのと同じこと)。粒度はリクエスト単位にする。接続ごとにリクエストは逐次処理されるので、1 回の write の staging → fill → rename の間に割り込むものがなく、call ごとに分けても得るものがない。

### write は書き込み先の symlink を辿らない

`fs.write` が書き込み先 (最終コンポーネント) の名前に置かれた symlink を辿ることは、どちらの経路でもしない。

- **staging + rename の経路**: 書き込み先を open しない。内容は `create_new` で作った自分専用の staging ファイルへ書き、`dir.rename` で書き込み先の**名前**を置き換える。rename は最終コンポーネントを辿らないため、名前に symlink があってもリンク先には触れない
- **in-place フォールバックの経路**: 書き込み先を open する唯一の場所で、しかも `truncate` する。ここは `follow(FollowSymlinks::No)` (unix では `O_NOFOLLOW` セマンティクス) で開き、名前に symlink があれば書き込みを拒否する。staging が失敗してからこの open までの間に symlink を置かれると、辿った先 (ルート外もあり得る) を空にしてしまうため。staging 経路の `create_new` が staging ファイル側で持っている防御と同等のものを、書き込み先側にも置く

`follow` は cap-std 本体の `OpenOptions` では非公開のため (`std` に対応する API が無いことを理由に隠されている)、`cap-fs-ext` の `OpenOptionsFollowExt` を使う。cap-std と同じ Bytecode Alliance の同バージョン系列の crate である。

no-follow の open が symlink を拒否した時のエラーは、OS が返すもの (unix では `ELOOP`「too many levels of symbolic links」) のままでは何が起きたか伝わらない (リンクは 1 つも辿られていない)。open が失敗した後に `dir.symlink_metadata` で名前を見て symlink だったかを判定し、`AlreadyExists` (staging 名に symlink が置かれていた時と同じ扱い) に置き換える。「後から見る」のは検査ではなく説明のためで、書き込みは既に行われていないため race にならない。`io::ErrorKind::FilesystemLoop` で判別しないのは、この variant が現行の Rust でまだ unstable なため。

### エラー表現

ルート外へ出た open の失敗は、既存の「outside the directory this daemon serves」(`permission_denied`) に正規化する (`root::confined_error`)。cap-std はこれを `io::ErrorKind::PermissionDenied` で返すが、OS が返す `EACCES` と kind が同じで区別できる専用の kind を持たない。実挙動では cap-std が自前で組み立てるエラー (`cap_primitives::fs::errors::escape_attempt`) は `raw_os_error()` が `None` で、OS 由来の拒否は `EACCES` を持つため、この 2 点で判別する。

### 検討して捨てた選択肢

**自前の `openat` / `O_NOFOLLOW` 逐次解決。** パスを 1 コンポーネントずつ `openat(dirfd, name, O_NOFOLLOW)` で降りていく実装。依存は増えないが、symlink をどこまで辿るか・`..` の扱い・ループ検出・Windows (`NtCreateFile` の相対 open と reparse point) の 3 OS 分を自前で持つことになる。これは cap-std がすでに解いている問題で、セキュリティ境界のコードを自作する理由がない。

**字句検査だけにする (canonicalize をやめ、symlink の追跡をしない)。** `Root::relative` の字句解決で止め、ハンドルを持たない案。TOCTOU は「検査したパスをそのまま open する」構造が残るため解消しない。ルート内の symlink がルート外を指していれば、字句的にはルート内なので通ってしまう。

**cap-std 採用の理由**は次の 3 点:

- Linux では `openat2(RESOLVE_BENEATH)`、それ以外では自前の逐次解決という切り分けを含め、macOS / Linux / Windows の 3 OS をこの crate が引き受ける (CI が回す 3 OS と一致する)
- ambient authority (プロセスが暗黙に持つ「名前でファイルシステム全体を引ける」権限) をライブラリが型で分離しており、`ambient_authority()` の呼び出し箇所を grep できる。デーモンではそれが `Root::new` の 1 箇所だけであることをコードで示せる
- Bytecode Alliance が WASI の実装基盤として維持している crate で、同じ「ディレクトリを capability として渡す」問題に対する参照実装になっている

## Consequences

- read / write / list の閉じ込めは、パスの検査ではなく open の各コンポーネントで行われるようになる。検査を通した後に経路を差し替えても、差し替え後の open が拒否される
- **絶対パスのリクエストはルートの字句 prefix で判定するようになる。** ルート内の実体を指していても、ルートの字句 prefix を持たない絶対パス (例: macOS の `/var/folders/...` は `/private/var/folders/...` に解決されるが、前者の綴りでは prefix にならない) は拒否される。従来は canonicalize してから比較していたため通っていた。クライアントは、デーモンが `daemon.root()` として報告するパスを prefix に持つ絶対パスか、相対パスを使う
- **ターゲットが絶対パスで書かれた symlink は、ルート内を指していても辿れない。** cap-std はルートのハンドルからの相対解決しか行わず、絶対パスの link target はハンドルの外を指すものとして拒否する。ルート内で完結させたい link は相対パスで張る
- **write は書き込み先の symlink のリンク先には決して書かない。** ルート内の symlink に write すると、staging + rename の経路では symlink 自体が通常ファイルに置き換わり (従来は canonicalize した実体側に書いていた)、in-place フォールバックの経路では書き込みが拒否される。リンク先を追いたいクライアントは実体パスへ write する。2 経路で結果が違うのは、staging 経路が書き込み先を open しない (名前を rename で置き換える) のに対し、フォールバック経路は書き込み先を open して truncate するため。両者に共通する不変条件は「write はリンク先に届かない」で、これはルート外を指す symlink への write が実体に届かないことの要請でもある
- **存在しないパスの判定タイミングが変わる。** 従来は `Root::resolve` の canonicalize が `not_found` を返していたが、字句解決はファイルシステムに問い合わせないため、`not_found` は実際の open が返すようになる。クライアントが受け取るエラーコードは変わらない
- **サーブ中のルートのリネーム・差し替えには追随しない。** データアクセスは起動時に開いたハンドルに固定され、元のディレクトリに対して動き続ける一方、fs.watch は字句パス (ADR 0002 の 4) を監視するため、差し替え後の新しいディレクトリ側を見る (または失敗する)。ルートをリネームできるのはルートの親に書ける者で、それは ADR 0001 の脅威モデルの外にある (その権限があればルートごと差し替えられる)。サーブ中のルートの付け替えはサポートせず、デーモンの再起動で行う
- ハンドル経由の操作は同期のため `spawn_blocking` に載る。`tokio::fs` と同じくブロッキングプールを消費するが、粒度がリクエスト単位になる分、1 リクエストあたりのプール往復は減る
- 依存が増える (`cap-std` と `cap-fs-ext`、およびその依存の `cap-primitives` / `rustix` / `io-lifetimes` / `io-extras` / `fs-set-times` / `maybe-owned` / `ambient-authority` / `ipnet`)。`crates/wim-core` は pure crate のままで、追加されるのは `wim-daemon` だけなので wasm32 ビルドには影響しない

## 参照

- issue: https://github.com/bannzai/wim/issues/31
- 発端のレビュー: PR #29 discussion_r3898835273、PR #69 (in-place フォールバックの open が symlink を辿る指摘)
- ADR 0001 (脅威モデル・プロトコル)、ADR 0002 (4: watch の字句解決、6: in-place フォールバック)
- cap-std: https://github.com/bytecodealliance/cap-std
