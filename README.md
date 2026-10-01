# hime

Rustの参照透過性を妨げる操作を探す、ソースコードベースの静的解析CLIです。
Rustコードを実行せず、`syn`で構文解析し、関数単位で結果を表示します。

## セットアップ

Rust edition 2024に対応したRustツールチェーンとCargoが必要です。
リポジトリのルートでビルドします。

```bash
cargo build --release --locked
```

コマンドとしてインストールする場合は、次を実行します。

```bash
cargo install --path . --locked
hime --help
```

## 使い方

インストールせずに試す場合は `cargo run --` の後に解析対象を指定します。
対象コードのコンパイルや実行は不要です。

```bash
# このリポジトリのサンプルを検査
cargo run -- fixtures/effects.rs

# 別のプロジェクトを検査
cargo run -- path/to/project/src

# 複数のファイルをまとめて検査
cargo run -- src/lib.rs src/main.rs

# JSON形式で出力し、判定不能も失敗として扱う
cargo run -- --strict --json path/to/project/src
```

インストール後は、たとえば `hime --strict path/to/project/src` と実行できます。

```text
hime [--json] [--strict] [PATH ...]
```

| オプション | 動作 |
| --- | --- |
| `--json` | 関数レポートの配列をJSONで標準出力へ出力 |
| `--strict` | `unknown` も終了コード `1` の対象にする |
| `--help`, `-h` | ヘルプを表示 |
| `--` | 以降の引数をパスとして扱う |

ファイルまたはディレクトリを複数指定できます。指定なしではカレントディレクトリの `src` を検査します。
ディレクトリは再帰的に探索し、拡張子が `.rs` のファイルを対象にします。
`target`・`.git`・シンボリックリンクは除外します。引数で直接指定したシンボリックリンクや `.rs` 以外のファイルは、警告を出してスキップします。
`src` と `./src` のように同じファイルを複数回指定しても、解析は1回です。

## 実行例

[fixtures/effects.rs](fixtures/effects.rs) には、計算・ローカル変数の更新・出力・時刻取得・可変参照・未解決の呼び出しの例があります。
出力の抜粋は次のとおりです。

```text
fixtures/effects.rs:1: square [Candidate]
fixtures/effects.rs:2: local_mutation [Candidate]
fixtures/effects.rs:3: output [Impure]
  fixtures/effects.rs:3:15: macro: macro println! may perform effects; expansion is unavailable
fixtures/effects.rs:4: indirect [Impure]
  fixtures/effects.rs:4:17: callee_effect: call to output propagates Impure
```

`indirect` は `output` を呼び出すため、その判定が伝播します。
サンプル全体では `2 candidate, 1 unknown, 4 impure` となり、終了コードは `1` です。
診断メッセージは英語で出力します。

## 結果

| 状態 | 意味 |
| --- | --- |
| `candidate` | 検出範囲内では副作用の兆候が見つからない |
| `unknown` | 呼び出し先や実行時の振る舞いを解決できない |
| `impure` | 副作用や外部状態への依存の疑いがある |

各診断にはファイル名・行・列・診断コード・理由が付きます。
JSONの `status` は小文字です。`impure` の判定を最優先にし、次に `unknown` を優先します。

## 検出対象

- 完全修飾された `std::fs`・`std::env`・`std::thread`・`rand`・`getrandom` 配下の呼び出し、および `std::io::stdout`・`std::net::TcpStream::connect`・`std::process::exit`・`std::time::SystemTime::now`・`std::time::Instant::now` などの既知の関数。`std::time::Duration::from_secs` のような値の生成は対象外です。
- `println!`・`dbg!` などの出力、および `panic!`・`unreachable!`・アサーションなど（`std::`・`core::` 付きも含む）。
- 引数・戻り値の型の最上位にある `&mut`、`&mut self`・`self: &mut Self`、静的変数へのアクセスの可能性。
- 同一ファイルの通常の関数呼び出し。インラインモジュール、ブロックで定義した関数、`crate::`・`self::`・`super::`、同一ファイル内を指す`use`（単一パス・グループ・別名・glob・再エクスポート）を扱い、呼び出しグラフの固定点まで状態を伝播します。impl・trait内の `f()` や `self::f()` は、外側のモジュールの関数を指します。

未解決の外部呼び出し、メソッド、関連関数（`Self::f`・`Type::f`・`Trait::f`）、関数ポインタ、シグネチャ内の共有参照や入れ子の `&mut`、マクロ、属性、unsafe、FFI、クロージャ、async、再帰は `unknown` とします。
ファイルの先頭より上を指す `super::` も、別ファイルを指すため `unknown` です。
ドキュメントコメント・`#[inline]`・`#[must_use]`・`#[allow]` などの実行内容を変えない属性は無視します。ファイル直下の`no_std`・`recursion_limit`などのクレート属性と、ドキュメント用の`feature(doc_cfg)`・`feature(doc_auto_cfg)`も無視します。`cfg_attr`は内側の属性がすべて無害な場合に限り無視し、`cfg`やその他の言語featureは`unknown`とします。ファイル・mod・impl・traitに付いたそれ以外の属性は、中の関数すべてを `unknown` にします。
関数・implメソッド・traitメソッドと、それらの本体やstruct/enumの型・判別子の式で定義した関数を検査します。ネストした関数の本体は、その宣言を含む関数の実行として扱いません。
implメソッドは `S::run`、traitの実装は `<S as T>::m` の形で表示します。
数値計算、ローカル変数の代入、`Some`・`Ok`・`Err` や同一ファイルのタプル構造体・列挙型バリアントの生成だけなら `candidate` になります。

## 終了コードとCI

| コード | 意味 |
| --- | --- |
| `0` | `impure` なし。通常モードでは `unknown` を許容 |
| `1` | `impure` あり。`--strict` では `unknown` も失敗 |
| `2` | オプション・入力・構文解析エラー、またはRustファイルなし |

```bash
cargo run -- --strict --json path/to/target/src
```

## 解析の限界

これは純粋性を証明するツールではありません。`--strict` が成功しても証明にはなりません。
参照透過性には、同じ入力から同じ結果が得られることと、観測可能な副作用がないことが必要です。
本ツールは、その条件に反する操作の兆候を構文から探します。

rustcの型情報・名前解決・借用解析は使いません。複数ファイル間の呼び出し、依存クレート、外部への`use`、マクロ展開、ビルドスクリプト、feature/cfgの実際の選択は解決しません。循環するimportや曖昧なglobも`unknown`になります。
条件付きコンパイルの各関数はソースにあるまま検査し、同名の関数定義が複数あれば呼び出しを未解決とします。同じ名前の候補がすべて値を生成するコンストラクタなら、定義が複数でも`candidate`として扱います。
完全修飾名も文字列のヒューリスティックなので、名前のシャドーイングにより誤検出・見逃しがあり得ます。

演算子のオーバーロード、暗黙の `Drop`、内部可変性、メモリ割り当て、整数オーバーフロー、終了性を一般には判定しません。
静的変数はファイル内の名前で保守的に照合するため、同名のローカル変数も誤検出する場合があります。
引数の `&mut` が未使用の場合、読み取り専用のファイル操作、決定的に失敗する `panic!` なども疑いとして報告します。
クロージャ・async内の操作は実行時期を区別せず報告します。
トップレベルのマクロが生成する関数や外部関数宣言は関数レポートに含まれません。

## ライブラリとして使う

同一Cargoプロジェクト内では `hime::analyze_source(file, source)` を呼び出せます。
別のローカルプロジェクトから使う場合は、依存関係にこのリポジトリのパスを指定します。

```toml
[dependencies]
hime = { path = "../hime" }
```

```rust
use hime::analyze_source;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let reports = analyze_source("example.rs", "fn square(x: i64) -> i64 { x * x }")?;
    for report in reports {
        println!("{}: {:?}", report.function, report.status);
    }
    Ok(())
}
```

## 開発と検証

```bash
cargo test --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
```

依存パッケージがローカルに揃っている環境では `--offline` を追加できます。

| ファイル | 役割 |
| --- | --- |
| [src/lib.rs](src/lib.rs) | 構文解析・診断・関数間の状態伝播 |
| [src/main.rs](src/main.rs) | ファイル探索・CLI・出力・終了コード |
| [tests/analysis.rs](tests/analysis.rs) | 解析と状態伝播のテスト |
| [tests/cli.rs](tests/cli.rs) | JSON出力と終了コードのテスト |
| [fixtures/effects.rs](fixtures/effects.rs) | 動作確認用のサンプル |
