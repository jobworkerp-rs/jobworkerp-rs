#!/usr/bin/env bash
# Usage: init-crate-submodules.sh [--list] [repo-dir]
#
# Checks out (shallowly) only the submodules the Cargo workspace builds from:
# those Cargo.toml names as a workspace member or a [patch] path. The other
# submodules (the admin UI, the MCP server sources) are not needed to build or
# to list the licenses of the Rust binaries. With --list, prints the selected
# paths instead.
#
# Kept compatible with the bash 3.2 shipped with macOS.
set -euo pipefail

list=0
if [[ "${1-}" == --list ]]; then
  list=1
  shift
fi
case "${1-}" in
  -*)
    printf 'ERROR: usage: %s [--list] [repo-dir]\n' "$0" >&2
    exit 2
    ;;
esac
repo=${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}

selected=()
while read -r _ path; do
  grep -Fq "\"${path}\"" "${repo}/Cargo.toml" && selected+=("${path}")
done < <(git config -f "${repo}/.gitmodules" --get-regexp '^submodule\..*\.path$')

# An empty pathspec would make `git submodule update` take every submodule.
if [[ "${#selected[@]}" -eq 0 ]]; then
  printf 'ERROR: Cargo.toml names none of the submodules in .gitmodules\n' >&2
  exit 1
fi
if [[ "${list}" -eq 1 ]]; then
  printf '%s\n' "${selected[@]}"
else
  git -C "${repo}" submodule update --init --depth 1 -- "${selected[@]}"
fi
