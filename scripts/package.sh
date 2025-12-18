#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [ ! -f target/release/mcosu-importer ]; then
  cargo build --release
fi

DIST="dist"
rm -rf "$DIST"
mkdir -p "$DIST/mcosu-importer"

cp target/release/mcosu-importer "$DIST/mcosu-importer"/
cp README.md CHANGELOG.md LICENSE "$DIST/mcosu-importer"/
cp -r assets "$DIST/mcosu-importer"/

echo "Bundle ready in $DIST/mcosu-importer"

if command -v zip >/dev/null 2>&1; then
  (cd "$DIST" && zip -r mcosu-importer.zip mcosu-importer >/dev/null)
  echo "Zip created at $DIST/mcosu-importer.zip"
fi
