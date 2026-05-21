#!/usr/bin/env bash
#
# release.sh — bump version, tag, publish to crates.io, create GitHub release.
#
# Usage:
#   ./release.sh 0.1.3          # explicit version
#   ./release.sh                # prompts for version
#
# What it does:
#   1. Updates the workspace version in Cargo.toml (+ Cargo.lock)
#   2. Commits the version bump
#   3. Tags as v$VERSION
#   4. Pushes commit + tag
#   5. Publishes crates in dependency order (with delays for index propagation)
#   6. Creates a GitHub release from the tag
#
# Prerequisites:
#   - cargo login (crates.io token)
#   - gh auth login (GitHub CLI)
#   - Clean working tree

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'

info()  { echo -e "${GREEN}==>${NC} $*"; }
warn()  { echo -e "${YELLOW}==> WARNING:${NC} $*"; }
fail()  { echo -e "${RED}==> ERROR:${NC} $*" >&2; exit 1; }

# ---------- preflight checks ----------

command -v cargo >/dev/null || fail "cargo not found"
command -v gh    >/dev/null || fail "gh (GitHub CLI) not found"

if [[ -n "$(git status --porcelain)" ]]; then
    fail "working tree is dirty — commit or stash first"
fi

CURRENT=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
info "current version: $CURRENT"

# ---------- get target version ----------

VERSION="${1:-}"
if [[ -z "$VERSION" ]]; then
    read -rp "new version (current: $CURRENT): " VERSION
fi
[[ -n "$VERSION" ]] || fail "no version provided"
[[ "$VERSION" != "$CURRENT" ]] || fail "version $VERSION is already current"

if git tag -l "v$VERSION" | grep -q .; then
    fail "tag v$VERSION already exists"
fi

info "releasing $CURRENT -> $VERSION"

# ---------- bump version ----------

# The workspace version in root Cargo.toml controls all crates.
# Also update the workspace.dependencies self-references.
info "bumping workspace version to $VERSION"
sed -i '' "s/^version = \"$CURRENT\"/version = \"$VERSION\"/" Cargo.toml
sed -i '' "s/version = \"$CURRENT\"/version = \"$VERSION\"/g" Cargo.toml

# Regenerate Cargo.lock
cargo check --workspace 2>/dev/null || cargo generate-lockfile

info "committing version bump"
git add Cargo.toml Cargo.lock
git commit -m "v$VERSION: bump workspace version"

# ---------- tag + push ----------

info "tagging v$VERSION"
git tag "v$VERSION"

info "pushing to origin"
git push origin main
git push origin "v$VERSION"

# ---------- publish to crates.io ----------
# Order matters: dependencies must be published before dependents.
# crates.io index propagation can take a few seconds, so we sleep
# between tiers.

TIER1=(culpert-macros)
TIER2=(culpert)
TIER3=(culpert-foundations culpert-tracing culpert-cli)

publish_crate() {
    local crate="$1"
    info "publishing $crate"
    cargo publish -p "$crate" || {
        warn "$crate publish failed — you may need to retry manually: cargo publish -p $crate"
        return 1
    }
}

for crate in "${TIER1[@]}"; do publish_crate "$crate"; done

info "waiting 30s for crates.io index to update..."
sleep 30

for crate in "${TIER2[@]}"; do publish_crate "$crate"; done

info "waiting 30s for crates.io index to update..."
sleep 30

for crate in "${TIER3[@]}"; do publish_crate "$crate"; done

# ---------- GitHub release ----------

info "creating GitHub release v$VERSION"
gh release create "v$VERSION" \
    --title "v$VERSION" \
    --generate-notes

info "done! https://github.com/rupert648/culpert/releases/tag/v$VERSION"
