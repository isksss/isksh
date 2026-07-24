#!/usr/bin/env bash
set -euo pipefail

cargo zigbuild --locked --release --target x86_64-unknown-linux-musl
cargo zigbuild --locked --release --target aarch64-unknown-linux-musl
cargo build --locked --release --target x86_64-pc-windows-gnu

mkdir -p dist
cp target/x86_64-unknown-linux-musl/release/isksh dist/isksh-linux-x86_64
cp target/aarch64-unknown-linux-musl/release/isksh dist/isksh-linux-aarch64
cp target/x86_64-pc-windows-gnu/release/isksh.exe dist/isksh-windows-x86_64.exe

for artifact in dist/isksh-linux-x86_64 dist/isksh-linux-aarch64 dist/isksh-windows-x86_64.exe; do
  sha256sum "$artifact" > "$artifact.sha256"
done
