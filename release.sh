#!/usr/bin/env bash
# release.sh — bump version + commit + push companion repo
# Usage:
#   ./release.sh patch        # 1.0.0 → 1.0.1
#   ./release.sh minor        # 1.0.0 → 1.1.0
#   ./release.sh major        # 1.0.0 → 2.0.0
#   ./release.sh 1.2.3        # explicit version

set -euo pipefail

BUMP="${1:-patch}"

# ── Read current version from Cargo.toml ────────────────────────────────────
CURRENT=$(grep '^version' Cargo.toml | head -1 | sed 's/.*= *"//' | sed 's/".*//')
echo "Current version: $CURRENT"

IFS='.' read -r MAJOR MINOR PATCH <<< "$CURRENT"

# ── Compute next version ─────────────────────────────────────────────────────
case "$BUMP" in
  major) MAJOR=$((MAJOR + 1)); MINOR=0; PATCH=0 ;;
  minor) MINOR=$((MINOR + 1)); PATCH=0 ;;
  patch) PATCH=$((PATCH + 1)) ;;
  [0-9]*.[0-9]*.[0-9]*) MAJOR=${BUMP%%.*}; REST=${BUMP#*.}; MINOR=${REST%%.*}; PATCH=${REST##*.} ;;
  *) echo "Usage: $0 [major|minor|patch|x.y.z]"; exit 1 ;;
esac

NEXT="$MAJOR.$MINOR.$PATCH"
echo "Next version:    $NEXT"

read -rp "Release v$NEXT? [y/N] " confirm
[[ "$confirm" =~ ^[Yy]$ ]] || { echo "Aborted."; exit 0; }

# ── Bump version in Cargo.toml (workspace version) ──────────────────────────
sed -i.bak "s/^version = \"$CURRENT\"/version = \"$NEXT\"/" Cargo.toml
rm -f Cargo.toml.bak

# ── Bump version in tauri.conf.json ─────────────────────────────────────────
sed -i.bak "s/\"version\": \"$CURRENT\"/\"version\": \"$NEXT\"/" src-tauri/tauri.conf.json
rm -f src-tauri/tauri.conf.json.bak

# ── Regenerate Cargo.lock with new version ────────────────────────────────────
cargo update -p remnant-finder-drive 2>/dev/null || true

echo "Bumped Cargo.toml + tauri.conf.json to $NEXT"

# ── Git commit & tag ─────────────────────────────────────────────────────────
git add Cargo.toml Cargo.lock src-tauri/tauri.conf.json
# Stage any other modified tracked files
git add -u

git commit -m "release: v$NEXT"
git tag -a "v$NEXT" -m "Release v$NEXT"

git push origin main
git push origin "v$NEXT"

echo ""
echo "✅ Released v$NEXT → https://github.com/DesignMaster-Solutions/RemnantFinder-Drive-Companion/releases/tag/v$NEXT"
