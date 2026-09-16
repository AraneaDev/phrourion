#!/usr/bin/env bash
set -euo pipefail

mkdir -p coverage
cargo llvm-cov --all-features --workspace --lcov --output-path coverage/lcov.info
cargo llvm-cov report --all-features --workspace
