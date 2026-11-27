#!/usr/bin/env bash
# Same checks as CI, then a release build.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
echo "Built target/release/claudewatch"
