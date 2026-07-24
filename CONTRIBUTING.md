# Contributing

変更はDocker開発環境内で行い、提出前に次を実行してください。

```console
docker compose run --rm dev mise run check-all
```

POSIXの挙動を追加・変更する場合は、根拠となる規格節をテスト名またはコメントに記録し、`POSIX-COMPATIBILITY.md`も更新してください。規格上未定義または実装依存の挙動を固定する比較テストは追加しません。

