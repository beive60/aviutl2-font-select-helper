# aviutl2-font-select-helper

AviUtl2 向けのフォント選択補助プラグインです。

## 開発環境

### 前提条件

- [Rust toolchain](https://rustup.rs/) — `x86_64-pc-windows-msvc` ターゲット
- [aviutl2-cli](https://github.com/sevenc-nanashi/aviutl2-cli) — ビルド・配置用 CLI（`au2` コマンド）

### セットアップ

```sh
git clone https://github.com/beive60/aviutl2-font-select-helper.git
cd aviutl2-font-select-helper
cargo binstall aviutl2-cli
au2 prepare
```

`au2 prepare` は AviUtl2 本体の取得・展開と、開発用ディレクトリへの成果物リンク準備を行います。

### 開発ビルド

```sh
au2 develop
```

### リリースビルド配置確認

```sh
au2 preview
```