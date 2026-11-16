#!/usr/bin/env bash
# Usage: generate-third-party-licenses.sh <out-file> [manifest]
#
# Writes the license listing of every third-party crate linked into the
# jobworkerp binaries for all release targets, using about.toml and about.hbs
# at the repository root. The default manifest is worker-main, whose
# dependency tree includes every distributed binary (grpc-front, mcp-server and
# ag-ui-front are its dependencies). Fails when a dependency has no
# recognizable license or one that about.toml does not accept, so a release can
# never ship a dependency whose license was not reviewed.
# Runs cargo-about from CARGO_ABOUT (see install-cargo-about.sh), otherwise as
# the `cargo about` subcommand, which cargo finds on PATH or in $CARGO_HOME/bin.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
out=${1-}
manifest=${2:-${ROOT}/worker-main/Cargo.toml}

if [[ -z "${out}" ]]; then
  printf 'ERROR: usage: %s <out-file> [manifest]\n' "$0" >&2
  exit 2
fi

if [[ -n "${CARGO_ABOUT-}" ]]; then
  cargo_about=("${CARGO_ABOUT}")
else
  cargo_about=(cargo about)
fi

"${cargo_about[@]}" generate --locked --fail \
  --config "${ROOT}/about.toml" \
  --manifest-path "${manifest}" \
  --output-file "${out}" \
  "${ROOT}/about.hbs"
