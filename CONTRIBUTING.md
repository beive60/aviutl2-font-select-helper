## コントリビューションガイド

Issue や Pull Request を歓迎します。開発時は以下の手順で環境をセットアップしてください。

## 開発環境のセットアップ

### 前提条件

- [Rust toolchain](https://rustup.rs/) — `x86_64-pc-windows-msvc` ターゲット
- [aviutl2-cli](https://github.com/sevenc-nanashi/aviutl2-cli) — `au2` コマンド

### セットアップ手順

```sh
git clone https://github.com/beive60/aviutl2-font-select-helper.git
cd aviutl2-font-select-helper
cargo binstall aviutl2-cli
au2 prepare
```

### 開発ワークフロー

通常の開発では `au2 develop` を使用して、デバッグビルドを開発用 AviUtl2 ディレクトリへ配置します。

```sh
au2 develop
```

リリースビルド相当での配置確認には `au2 preview` を使用します。

```sh
au2 preview
```

## フォーマットとリント

PR 提出前に以下を実行してください。

```sh
cargo fmt
cargo clippy -- -D warnings
cargo test
```
