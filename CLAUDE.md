# malchut

Markdown からスライドを作って発表する GTK4 アプリ。仕様と設計判断は `docs/design.md` にある。
構文や挙動を変えたら、`docs/design.md` と `README.md` も合わせて直すこと。

## コマンド

```sh
cargo build
cargo test
cargo clippy --all-targets            # 警告なしを保つ
INSTA_UPDATE=always cargo test        # パーサのスナップショットを更新する
cargo run -- examples/demo.md         # GUI を起動する
cargo run -- export examples/demo.md --png-dir /tmp/out   # 描画結果を PNG で確認する
```

`examples/demo.md` は全機能のサンプルを兼ねる。機能を足したら、スライドを 1 枚足す。

## 構成

```
Markdown ─parse→ Deck ─layout(theme)→ DisplayList ─paint→ Cairo
```

- `crates/malchut-core`: GTK に依存しない中核。ヘッドレスでテストできる。
  - `parse.rs`: comrak の AST から `Deck` を組み立てる。構文を知っているのはここだけ。
  - `model.rs`: `Deck` / `Slide` / `Block` などの型。
  - `theme.rs`: TOML のテーマと、クラスごとのスタイル解決。
  - `layout.rs`: `Slide` を論理座標上の `DisplayList` にする。
  - `paint.rs`: `DisplayList` を Cairo に描く。
  - `export.rs`: PDF と PNG の書き出し。
  - `highlight.rs` / `diagram.rs` / `picture.rs`: コードハイライト、Mermaid、画像。
- `crates/malchut`: GTK4 アプリと CLI。
  - `state.rs`: `AppState` と `Action`。ウィンドウに依存しない状態のすべて。
  - `ui.rs`: ウィンドウ、キー操作、クリック処理。
  - `watch.rs`: ファイル監視。

## 守ること

- **座標は論理座標**(16:9 なら 1920×1080、高さは常に 1080)で統一する。
  表示先ごとの違いは `cr.scale` だけにし、レイアウト結果を表示先に依存させない。
- **`model.rs` の型は `Hash` と `Eq` を持たせる**。レイアウト結果のキャッシュのキーと、
  変更されたスライドの検出に使っている。小数は `Num` で包む。
- **テーマの項目を足すときは `themes/default.toml` に既定値を書く**。
  `Style` は serde の既定値を持たず、既定テーマに差分を重ねた結果を読む。
- **未知の指定は黙って捨てず、警告(`Diagnostic`)にする**。
- **描画時に失敗しうるもの(画像、図)は、その場所にメッセージを出して続行する**。
  アプリ全体は止めない。
- **GUI の状態は `AppState` に集約する**。状態を変えるのは `dispatch` 経由だけにし、
  ウィンドウは `refresh` で状態を映すだけにする。
  描画コールバックの中で `state.borrow_mut()` を取らない。
- **構文の拡張機構は作らない**。汎用の属性層やプラグインの仕組みは、意図的に持っていない。

## GUI の確認

GUI は Xvfb 上で動かして確認する(GTK の X11 バックエンドを使う)。

```sh
Xvfb :99 -screen 0 1920x1080x24 &
export DISPLAY=:99 GDK_BACKEND=x11 GSK_RENDERER=cairo; unset WAYLAND_DISPLAY
target/debug/malchut examples/demo.md &
WIN=$(xdotool search --name 'demo.md' | head -1)
xdotool key --window $WIN Right      # キー入力
xdotool getwindowname $WIN           # "demo.md (2 / 16) [default]" で位置とテーマが分かる
import -window $WIN shot.png         # スクリーンショット
```

- ウィンドウマネージャが無いので、別ウィンドウを閉じたあとは `xdotool windowfocus` が必要。
- 外部リンクはクリックしない。ユーザーのブラウザが開く。
- モニタ指定の全画面化、画面入れ替え、アイドル抑止は Xvfb では確認できない。
  変更した場合は、実機での確認が必要だと伝える。

## 落とし穴

- `---` の直前に空行が無いと、comrak は setext 見出しとして解釈する。
- Pango のテキスト位置は、マークアップを除いたテキストのバイト位置で扱う。
  `MarkupBuilder` でテキストを出力するときは `plain_len` も必ず進める(リンクの範囲がずれる)。
- Cairo の PDF リンク(`tag_begin` の `rect`)には `cr.scale` が効かない。
  `export.rs` で `PDF_SCALE` を掛けている。
- merman には `resvg-safe` パイプラインを指定する。既定の出力はラベルが `foreignObject` で、
  librsvg では文字が消える。
- gtk-rs 系のクレート(gtk4, cairo-rs, pango, gio, glib, gdk-pixbuf, librsvg)は
  同じ世代で揃える。`cargo tree -d` で cairo-rs が 2 つ出たら型が合わなくなる。

## 書き方

- コードコメント、`docs/` 以下、ユーザー向けメッセージは日本語で書く。
- `README.md` とコミットメッセージは英語で書く。
- コミットは Conventional Commits に従い、本文には変更の理由を書く。
