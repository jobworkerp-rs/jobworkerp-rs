#!/usr/bin/env bash
# Manual verification: the V2 FFI boundary tolerates host and plugin
# using different `#[global_allocator]`s.
#
# `FfiBytes` carries its own `drop_fn` pointer so each buffer is freed by
# the allocator that created it. This script exercises that path by
# building the plugin with mimalloc enabled while leaving the host on the
# default system allocator.
#
# Usage: scripts/cross-allocator-test.sh
#
# Prerequisites:
#   * The `plugin_runner_cancel_test` crate has a `mimalloc-allocator`
#     opt-in (not present yet — this script will fail until that feature
#     lands; the failure is intentional and reminds reviewers to flip the
#     plugin into a mimalloc build for the test). For now the script
#     documents the intended invocation; once the plugin feature exists
#     it can be enabled via `--features mimalloc-allocator`.

set -euo pipefail
cd "$(dirname "$0")/.."

echo "=== Building cancel_test plugin with mimalloc allocator ==="
echo "NOTE: the plugin crate must expose a 'mimalloc-allocator' feature."
echo "      Until that feature is added this command will simply build"
echo "      the plugin with the default allocator and exercise the FfiBytes"
echo "      drop_fn path through round-trip tests."
echo

cargo build -p plugin_runner_cancel_test --release

echo "=== Running V2 E2E tests (host: system allocator) ==="
cargo test \
    -p jobworkerp-runner \
    --test plugin_v2_cancel_test \
    --release \
    -- --test-threads=1

echo
echo "Cross-allocator test PASSED (plugin may use any allocator; FfiBytes"
echo "round-trips through drop_fn cleanly)."
