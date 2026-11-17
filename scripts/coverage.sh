#!/bin/bash
# Test coverage summary for the whole workspace (H24, D15). Prints per-file line coverage; sets no
# threshold. Extra arguments go to cargo llvm-cov (for example --html). Exit 2 if cargo-llvm-cov
# is missing (fail closed, as the other scripts do).
if ! cargo llvm-cov --version >/dev/null 2>&1; then
  echo "coverage: cargo-llvm-cov is not installed. Install it with: cargo install cargo-llvm-cov --locked (and: rustup component add llvm-tools-preview)." >&2
  exit 2
fi
cargo llvm-cov --workspace --summary-only "$@"
