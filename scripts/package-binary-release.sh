#!/usr/bin/env bash

set -euo pipefail

usage() {
  cat <<'EOF'
Usage: ./scripts/package-binary-release.sh <version> <target>

Examples:
  ./scripts/package-binary-release.sh 20260320.01 aarch64-apple-darwin
  ./scripts/package-binary-release.sh 20260320.01 x86_64-unknown-linux-gnu

This script packages a standalone release binary built with embedded assets and
embedded WASM components into dist/moltis-<version>-<target>.tar.gz.
EOF
}

if [[ $# -ne 2 ]]; then
  usage >&2
  exit 2
fi

version="$1"
target="$2"
binary_path="target/${target}/release/moltis"
package_root="moltis-${version}-${target}"
dist_dir="dist"
stage_dir="${dist_dir}/${package_root}"
archive_path="${dist_dir}/${package_root}.tar.gz"
checksum_path="${archive_path}.sha256"

if [[ ! -f "$binary_path" ]]; then
  echo "release binary not found: $binary_path" >&2
  exit 1
fi

rm -rf "$stage_dir"
mkdir -p "$stage_dir"

cp "$binary_path" "${stage_dir}/moltis"
cp README.md "${stage_dir}/README.md"
cp LICENSE.md "${stage_dir}/LICENSE.md"
cp examples/moltis.toml.example "${stage_dir}/moltis.toml.example"

tar czf "$archive_path" -C "$dist_dir" "$package_root"
(cd "$dist_dir" && shasum -a 256 "$(basename "$archive_path")" > "$(basename "$checksum_path")")

echo "Created:"
echo "  $archive_path"
echo "  $checksum_path"
