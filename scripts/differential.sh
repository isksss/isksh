#!/usr/bin/env bash
set -euo pipefail

cargo build --locked --quiet
isksh=target/debug/isksh
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT

cases=(
  "value=world; printf '%s\\n' \"\$value\""
  "for value in a b c; do printf '<%s>' \"\$value\"; done"
  "unset value; printf '%s' \"\${value:-fallback}\""
  "if false; then printf no; elif true; then printf yes; else printf no; fi"
  "value=7; printf '%s' \"\$((value * 6))\""
  "value=abcabc; printf '%s|%s|%s|%s' \"\${value%c*}\" \"\${value%%c*}\" \"\${value#a*}\" \"\${value##a*}\""
  "IFS=:; value='a::b:'; for field in \$value; do printf '<%s>' \"\$field\"; done"
  "yes | head -n 1"
)

for index in "${!cases[@]}"; do
  source=${cases[$index]}
  dash -c "$source" > "$temporary/dash-$index.out" 2> "$temporary/dash-$index.err"
  dash_status=$?
  "$isksh" -c "$source" > "$temporary/isksh-$index.out" 2> "$temporary/isksh-$index.err"
  isksh_status=$?
  test "$dash_status" = "$isksh_status"
  cmp "$temporary/dash-$index.out" "$temporary/isksh-$index.out"
done
