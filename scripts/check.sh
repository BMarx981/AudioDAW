#!/usr/bin/env bash
# Run stages 1-4 of CI locally. Run before pushing.
# Stage 5 (integration tests) is too slow for the inner loop.

set -euo pipefail

# Colors
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m'

step() {
  echo ""
  echo -e "${YELLOW}==> $1${NC}"
}

# Stage 1: Static checks
step "Rust format check"
cargo fmt --all -- --check

step "Rust clippy"
cargo clippy --workspace --all-targets -- -D warnings

step "Dart format check"
(cd app && dart format --output=none --set-exit-if-changed .)

step "Flutter analyze"
(cd app && flutter analyze)

# Stage 2: Bridge regeneration check
step "Bridge regeneration check"
flutter_rust_bridge_codegen generate
if ! git diff --exit-code --quiet; then
  echo -e "${RED}Bridge code is out of sync. Commit the regenerated files.${NC}"
  exit 1
fi

# Stage 3: Rust tests
step "Rust tests"
cargo test --workspace --all-features

# Stage 4: Flutter widget tests
step "Flutter widget tests"
(cd app && flutter test)

echo ""
echo -e "${GREEN}All checks passed.${NC}"
