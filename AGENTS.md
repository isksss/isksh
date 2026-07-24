# AGENTS.md

このファイルは、`isksh`リポジトリを変更するコーディングエージェント向けの作業規約です。

## プロジェクト概要

- 製品名、Cargoパッケージ名、バイナリ名は`isksh`。
- RustでPOSIX.1-2024 Shell Command Language互換シェルを実装する。
- Windows、macOS、Linuxで利用できる単体バイナリを目標とする。
- POSIX完全準拠やBash完全互換を根拠なく表明しない。
- 互換性の差異は`POSIX-COMPATIBILITY.md`と`README.md`へ記録する。

## 言語と文書

- ユーザーへの回答、README、設計文書、コードコメントは原則として日本語で記述する。
- 識別子と公開CLIの語彙は既存コードに合わせて英語を使用する。
- UTF-8、LFを使用する。シェル入力としてはLFとCRLFの両方を維持する。

## 開発環境

- 開発、ビルド、Linuxテスト、クロスターゲット検査はDockerコンテナ内で行う。
- ホストへRustやクロスコンパイラを追加しない。
- ツールのバージョンは`mise.toml`に従い、個別に上書きしない。
- 通常のコマンド形式は次のとおり。

```console
docker compose run --rm dev mise run <task>
```

- `Cargo.toml`または`Cargo.lock`を変更する操作も原則としてコンテナ内で実行する。
- Windows実機テストが必要な場合に限り、生成済みEXEをWindowsホストで実行する。
- macOSは実機がないため、x86_64・aarch64両方の`cargo check`までを必須とする。

## 必須検証

変更内容に応じたテストを追加し、完了前に次を実行する。

```console
docker compose run --rm dev mise run check-all
```

`check-all`には次が含まれる。

- rustfmt差分ゼロ
- Clippy警告ゼロ
- 全Rustテスト成功
- dashとのPOSIX差分テスト成功
- 本体ライブラリの行カバレッジ100%
- Linux x86_64/aarch64、Windows x86_64、macOS x86_64/aarch64のターゲット確認
- Linux完全静的リンクとWindows依存DLLの検証

Windows固有の実行処理を変更した場合は、Windowsホストで次も実行する。

```powershell
.\scripts\windows-smoke.ps1
```

## 実装方針

- lexer、parser、AST、展開器、実行器、OS抽象化の責務を混在させない。
- POSIX動作を変更する場合は、該当する規格箇所を確認してテストへ根拠を残す。
- Bash拡張はGNU Bash Reference Manualおよび`bash help`の動作を基準にする。
- POSIXで未定義の挙動をdashとの差分テストへ固定しない。
- 新しい構文はlexer/parserの単体テストと実行統合テストを追加する。
- 構文エラーには可能な限り行・列情報と、不完全入力か無効入力かの区別を保持する。
- スクリプト、変数、コマンド置換結果はUTF-8方針を維持する。
- エラーや境界条件を含め、本体ライブラリの行カバレッジ100%を維持する。

## 依存関係

- C/C++共有ライブラリを要求するクレートを追加しない。
- pure Rust、またはOS APIを直接使用するクレートを優先する。
- 新規依存は静的リンク、Windows GNU、Linux musl、macOS両ターゲットで利用可能か確認する。
- 依存追加後は`Cargo.lock`を更新し、`check-targets`と`verify-static`を必ず実行する。

## プラットフォーム要件

### Linux

- `x86_64-unknown-linux-musl`と`aarch64-unknown-linux-musl`を維持する。
- リリースELFに動的依存を追加しない。

### Windows

- `x86_64-pc-windows-gnu`と静的CRTを維持する。
- Windows標準システムDLL以外を実行時依存に追加しない。
- `PATH`、`PATHEXT`、`.cmd`、`.bat`、CRLFの既存動作を壊さない。
- PowerShellスクリプトを暗黙に実行しない。

### macOS

- `x86_64-apple-darwin`と`aarch64-apple-darwin`の`cargo check`を維持する。
- macOS標準ライブラリへの動的参照は許容する。
- 実機で確認していない動作を検証済みと記載しない。

## 配布要件

- リリース成果物はOS・アーキテクチャ別の単体バイナリとSHA-256チェックサムのみとする。
- 外部ユーティリティ、設定ファイル、追加ランタイムをバイナリへ同梱しない。
- `dist/`の生成物をソース変更としてコミットしない。

## Gitと変更管理

- ユーザーの既存変更を上書き、破棄、巻き戻ししない。
- 無関係なファイルを変更しない。
- コミット前に`git diff --check`と`mise run check-all`の成功を確認する。
- コミットはユーザーから依頼された場合に行う。
- 互換性や既知の制約が変化した場合は、コードと同じコミットで文書を更新する。
