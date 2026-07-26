# コントリビューションガイド

この文書は、外部コントリビューター、保守担当者、コーディングエージェントが`isksh`を変更する際の標準手順を定めます。GitHub上ではPull Requestと呼ぶため、Merge Requestを含めて最初に「Pull Request（PR/MR）」と表記し、以降は「PR」と表記します。

## 開発環境

開発、ビルド、Linuxテスト、クロスターゲット検査はDockerコンテナ内で行います。ホストへRustやクロスコンパイラを追加せず、ツールのバージョンは`mise.toml`に従ってください。

```console
docker compose build dev
docker compose run --rm dev mise run build
```

`Cargo.toml`または`Cargo.lock`を更新する操作も、原則としてコンテナ内で実行します。Windows固有の実行処理を変更した場合に限り、生成済みEXEをWindowsホストで検証します。

## ブランチ

作業前に`main`を最新化し、`type/short-summary`形式のブランチを作成します。`short-summary`には変更内容を表す英小文字のkebab-caseを使用してください。

`main`へのcommitとpushは禁止します。すべての変更は作業ブランチへcommitし、PRを通して`main`へ取り込んでください。

```console
git switch main
git pull --ff-only
git switch -c feat/zsh-autoload
```

使用できるtypeは次のとおりです。

| type | 用途 | 例 |
|---|---|---|
| `feat` | 機能追加 | `feat/zsh-autoload` |
| `fix` | 不具合修正 | `fix/windows-path` |
| `test` | テストのみの変更 | `test/job-control` |
| `docs` | 文書のみの変更 | `docs/contribution-guide` |
| `refactor` | 動作を変えない内部整理 | `refactor/parser-state` |
| `ci` | CIや自動化の変更 | `ci/macos-release` |
| `chore` | 保守作業 | `chore/update-toolchain` |
| `release` | リリース準備 | `release/v0.6.0` |

1つのブランチには1つの目的だけを含めます。

## 実装と文書

- 変更内容に応じた単体テスト、統合テスト、回帰テストを追加します。
- POSIXの挙動を追加または変更する場合は、根拠となる規格節をテスト名またはコメントへ記録し、`POSIX-COMPATIBILITY.md`も更新します。
- 規格上未定義または実装依存の挙動を固定する比較テストは追加しません。
- ソースコードのコメントとREADME以外の文書は、すべて日本語で記述します。
- READMEを変更する場合は、英語の`README.md`を主文書とし、`README.ja.md`と`README.zh-CN.md`も同じ内容へ更新します。
- 互換性や既知の制約が変わる場合は、実装と同じPRで文書を更新します。
- `dist/`の成果物はコミットしません。

## 検証

コミット前に全検証を実行します。

```console
docker compose run --rm dev mise run fmt-markdown
docker compose run --rm dev mise run check-all
git diff --check
```

`fmt-markdown`は全Markdownファイルを整形します。`check-all`はRustとMarkdownの整形検査、Clippy、Markdown lint、全テスト、差分テスト、100%の行・関数カバレッジ、対応ターゲットの検査、リリースバイナリの静的依存検証を実行します。

Windows固有の実行処理を変更した場合は、Windowsホストでも次を実行します。

```powershell
.\scripts\windows-smoke.ps1
```

検証できなかった項目がある場合は、理由と残るリスクをPR本文へ記載します。

## コミット

コミットメッセージは`[prefix] 日本語の要約`形式にします。要約は命令形相当の簡潔な日本語とし、末尾に句点を付けません。1コミットには1つの目的だけを含めます。

```console
git commit -m "[add] zshのautoload対応を追加"
```

| prefix | 用途 | 対応するブランチtype |
|---|---|---|
| `add` | 機能追加 | `feat` |
| `fix` | 不具合修正 | `fix` |
| `test` | テストのみの変更 | `test` |
| `docs` | 文書のみの変更 | `docs` |
| `refactor` | 動作を変えない内部整理 | `refactor` |
| `ci` | CIや自動化の変更 | `ci` |
| `chore` | 保守作業 | `chore` |
| `release` | リリース準備 | `release` |

Conventional Commits形式は使用しません。複数の目的がある場合は、目的ごとにコミットを分けてください。

## Pull Request

作業ブランチをpushし、`main`宛てのPRを作成します。

```console
git push -u origin feat/zsh-autoload
```

- PRタイトルにもコミットと同じ`[prefix] 日本語の要約`形式を使用します。
- `.github/pull_request_template.md`の各項目を埋め、関連Issueがある場合は番号を記載します。
- 変更内容、検証結果、互換性への影響、未検証事項を明記します。
- 作業ブランチへ最新の`main`を取り込み、LinuxとWindowsの必須CIをすべて成功させます。
- レビュー指摘を反映し、すべてのレビュー会話を解決します。
- 承認後はsquash mergeし、squashコミットの件名をPRタイトルと一致させます。
- マージ後は作業ブランチを削除します。

## リリース

リリース番号はSemantic Versioningに従います。

- major: 後方互換性のない変更
- minor: 後方互換性を保った機能追加
- patch: 後方互換性を保った不具合修正

リリースは次の手順で行います。

1. `main`から`release/vX.Y.Z`ブランチを作成します。
2. `Cargo.toml`と`Cargo.lock`のパッケージ版数を同じ`X.Y.Z`へ更新します。
3. 必須検証を実行し、`[release] vX.Y.Zを準備`というタイトルのPRを作成します。
4. PRのCI成功とレビュー完了後、`main`へsquash mergeします。
5. 更新後の`main`でCIが成功したことを確認します。
6. squashコミットへ注釈付きタグを作成してpushします。

```console
git switch main
git pull --ff-only
git tag -a vX.Y.Z -m "isksh vX.Y.Z"
git push origin vX.Y.Z
```

7. Release CIで、全OSのビルドと実行確認、crates.io公開、GitHub Release作成、Linux・Windows・macOSでのaqua導入確認がすべて成功するまで監視します。
8. GitHub Releaseに各OS・アーキテクチャのバイナリとSHA-256チェックサムが揃っていることを確認します。

`main`への直接pushは禁止しますが、リリースを開始する`vX.Y.Z`タグのpushは許可します。

公開したタグは削除やforce更新をしません。公開後に問題が見つかった場合は、修正を`main`へ取り込み、次のpatch版としてリリースします。
