#!/usr/bin/env bash
set -euo pipefail

cargo check --locked --target x86_64-unknown-linux-musl
cargo check --locked --target aarch64-unknown-linux-musl
cargo check --locked --target x86_64-pc-windows-gnu
cargo check --locked --target aarch64-apple-darwin
