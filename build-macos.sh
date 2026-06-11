#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"

if ! command -v cargo >/dev/null 2>&1; then
  echo "Rust/Cargo not found. Install from https://rustup.rs then re-run."
  exit 1
fi

if ! command -v node >/dev/null 2>&1; then
  echo "Node.js not found."
  exit 1
fi

npm install

if [[ ! -f src-tauri/icons/icon.icns ]]; then
  echo "Generating icons from app-icon-source.png..."
  npm run icons
fi

npm run build

echo ""
echo "Done. DMG (macOS):"
ls -1 target/release/bundle/dmg/*.dmg 2>/dev/null || true
echo "App bundle:"
ls -1d target/release/bundle/macos/*.app 2>/dev/null || true
