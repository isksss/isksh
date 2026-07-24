#!/usr/bin/env bash
set -euo pipefail

for binary in \
  target/x86_64-unknown-linux-musl/release/isksh \
  target/aarch64-unknown-linux-musl/release/isksh; do
  file "$binary" | grep -q 'statically linked'
  if readelf -d "$binary" 2>/dev/null | grep -q '(NEEDED)'; then
    echo "$binary has a dynamic dependency" >&2
    exit 1
  fi
done

pe=target/x86_64-pc-windows-gnu/release/isksh.exe
file "$pe" | grep -q 'PE32+'
imports=$(llvm-objdump -p "$pe" | awk '/DLL Name:/ {print toupper($3)}')
allowed='^(KERNEL32\.DLL|NTDLL\.DLL|USER32\.DLL|USERENV\.DLL|SHELL32\.DLL|COMBASE\.DLL|WS2_32\.DLL|ADVAPI32\.DLL|BCRYPT\.DLL|BCRYPTPRIMITIVES\.DLL|MSVCRT\.DLL|API-MS-WIN-.*\.DLL)$'
while IFS= read -r dll; do
  if [[ -n "$dll" ]] && ! [[ "$dll" =~ $allowed ]]; then
    echo "Unexpected Windows DLL dependency: $dll" >&2
    exit 1
  fi
done <<< "$imports"

test_dir=$(mktemp -d)
trap 'rm -rf "$test_dir"' EXIT
cp target/x86_64-unknown-linux-musl/release/isksh "$test_dir/isksh"
actual=$(cd "$test_dir" && ./isksh -c 'value=standalone; printf "%s" "$value"')
test "$actual" = standalone
