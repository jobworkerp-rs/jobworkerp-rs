#!/usr/bin/env bash
# Manual verification: the V2 FFI boundary tolerates host and plugin
# being compiled with different rustc versions.
#
# This script is intentionally not wired into CI yet. The goal is to make
# the test runnable locally with a single command so reviewers can verify
# the ABI claim without setting up a matrix build.
#
# Usage: scripts/cross-rustc-test.sh [HOST_TOOLCHAIN] [PLUGIN_TOOLCHAIN]
#
# Defaults: host=stable, plugin=nightly
#
# Prerequisites:
#   * `rustup` with both toolchains installed (`rustup toolchain install nightly`).
#   * Both toolchains use the same target triple as the current machine.

set -euo pipefail

HOST_TOOLCHAIN="${1:-stable}"
PLUGIN_TOOLCHAIN="${2:-nightly}"

cd "$(dirname "$0")/.."

echo "=== Building plugin with rustup toolchain '${PLUGIN_TOOLCHAIN}' ==="
cargo "+${PLUGIN_TOOLCHAIN}" build -p plugin_runner_cancel_test --release

echo "=== Running V2 E2E tests with rustup toolchain '${HOST_TOOLCHAIN}' ==="
cargo "+${HOST_TOOLCHAIN}" test \
    -p jobworkerp-runner \
    --test plugin_v2_cancel_test \
    --release \
    -- --test-threads=1

echo
echo "Cross-rustc test PASSED (host=${HOST_TOOLCHAIN}, plugin=${PLUGIN_TOOLCHAIN})."
