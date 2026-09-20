#!/usr/bin/env bash
# Full local gate. Same steps CI runs. Fails fast.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== fmt =="
cargo fmt --all -- --check

echo "== check =="
cargo check --workspace --all-targets

echo "== clippy =="
cargo clippy --workspace --all-targets -- -D warnings

echo "== test =="
cargo test --workspace

echo "== driver smoke =="
cargo run -q -- check examples/hello.vl
cargo run -q -- build examples/arith.vl --emit lir | head -20
cargo run -q -- check examples/capabilities.vl
cargo run -q -- build examples/capabilities.vl --emit lir | head -20
if cargo run -q -- check examples/err_undefined.vl; then
  echo "ERROR: err_undefined.vl should fail" >&2
  exit 1
fi
if cargo run -q -- check examples/err_readonly_mutation.vl; then
  echo "ERROR: err_readonly_mutation.vl should fail" >&2
  exit 1
fi

echo "ALL GREEN"
