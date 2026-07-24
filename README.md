# isksh

`isksh`はRustで実装する、POSIX.1-2024 Shell Command Language互換を目標としたクロスプラットフォームシェルです。

現在はMVP開発段階であり、POSIX完全準拠ではありません。対応状況は[POSIX-COMPATIBILITY.md](POSIX-COMPATIBILITY.md)を参照してください。

## 開発

ホストにはDockerだけが必要です。Rustと関連ツールはmiseによってコンテナ内へ導入されます。

```console
docker compose run --rm dev mise run build
docker compose run --rm dev mise run test
docker compose run --rm dev mise run check-all
```

Dev Containerから同じ環境を利用することもできます。

`mise run build-release`は`dist/`へLinux x64/arm64とWindows x64の単体バイナリおよびSHA-256チェックサムを生成します。Windows実行確認はホスト側で次を実行します。

```powershell
.\scripts\windows-smoke.ps1
```

## 使用方法

```console
isksh script.sh arg1 arg2
isksh -c 'name=world; printf "hello %s\n" "$name"'
printf 'echo hello\n' | isksh -s
```

スクリプトと変数はUTF-8です。Windowsではパス区切りに`/`を推奨します。外部ユーティリティは同梱せず、実行環境の`PATH`から探索します。

## ライセンス

MITまたはApache License 2.0のいずれかを選択できます。
