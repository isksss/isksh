#!/usr/bin/env bash
set -euo pipefail

version=${1:?usage: aqua-smoke.sh vX.Y.Z}
if [[ ! $version =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "invalid release version: $version" >&2
  exit 2
fi

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT

cp "$repo_root/aqua/aqua.yaml" "$repo_root/aqua/registry.yaml" "$work_dir/"
export AQUA_ROOT_DIR="$work_dir/root"
export AQUA_DISABLE_POLICY=true
export AQUA_CONFIG="$work_dir/aqua.yaml"

aqua g -i "local,isksss/isksh@$version"
aqua update-checksum
aqua install

output=$("$AQUA_ROOT_DIR/bin/isksh" -c 'printf aqua-ok')
test "$output" = aqua-ok
