#!/usr/bin/env bash
# Usage: generate-license-listing.sh <out-file>
#
# Writes THIRD-PARTY-LICENSES.txt (scripts/generate-third-party-licenses.sh)
# with the pinned cargo-about, which it installs into a temporary directory
# unless CARGO_ABOUT already names one. The one step every workflow that ships
# the listing (release archives, container images) runs.
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
out=${1-}
if [[ -z "${out}" ]]; then
  printf 'ERROR: usage: %s <out-file>\n' "$0" >&2
  exit 2
fi

if [[ -z "${CARGO_ABOUT-}" ]]; then
  tools=$(mktemp -d)
  trap 'rm -rf "${tools}"' EXIT
  # Not exported to later workflow steps: the installation is removed on exit.
  GITHUB_ENV='' bash "${SCRIPT_DIR}/install-cargo-about.sh" "${tools}"
  CARGO_ABOUT="${tools}/cargo-about"
fi
CARGO_ABOUT="${CARGO_ABOUT}" bash "${SCRIPT_DIR}/generate-third-party-licenses.sh" "${out}"
