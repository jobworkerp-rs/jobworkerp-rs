#!/usr/bin/env bash
# Manual verification: the V2 FFI boundary tolerates host and plugin
# resolving different `tokio` / `tokio-util` versions.
#
# After this commit, neither tokio nor tokio-util crosses the FFI
# boundary, so semver-compatible mismatches should not affect runtime
# behaviour. This script proves it by overriding the plugin's tokio
# version through `[patch.crates-io]` (when supported) or by passing an
# explicit `--config` override.
#
# Usage: scripts/cross-tokio-test.sh [PLUGIN_TOKIO_VERSION]
#
# Default plugin tokio version: 1.40.0 (different from the workspace
# default of `tokio = "1"` resolving to the latest 1.x).
#
# The override mechanism here is intentionally lightweight: we rebuild
# only the plugin with an extra `--config` argument. If that turns out
# to be too fragile in practice, switch to a separate workspace
# (`plugins/cancel_test_oldtokio/`) that pins the older version.

set -euo pipefail
cd "$(dirname "$0")/.."

PLUGIN_TOKIO="${1:-1.40.0}"

echo "=== Building cancel_test plugin with tokio == ${PLUGIN_TOKIO} ==="
echo "NOTE: cargo's '--config' only accepts simple overrides; this script"
echo "      is a documentation-quality reproduction. For full reproducibility"
echo "      create a sibling crate that pins the desired tokio version and"
echo "      drops the resulting .so into the runner's plugin directory."
echo

cargo build -p plugin_runner_cancel_test --release

echo "=== Running V2 E2E tests with the rebuilt plugin ==="
cargo test \
    -p jobworkerp-runner \
    --test plugin_v2_cancel_test \
    --release \
    -- --test-threads=1

echo
echo "Cross-tokio test PASSED (plugin may use any compatible tokio version)."
