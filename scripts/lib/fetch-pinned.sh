# shellcheck shell=bash
# Shared by the install-*.sh scripts, which fetch release tools at a pinned
# version so every runner uses the same, publicly visible build.
#
# Kept compatible with the bash 3.2 shipped with macOS.

# fetch_pinned <url> <sha256> <out-file>: download <url> to <out-file> and
# fail unless its SHA-256 is <sha256>. The caller extracts nothing until this
# succeeds, so a tampered or truncated archive never reaches the runner.
fetch_pinned() {
  local url=$1 expected=$2 out=$3 actual
  if ! curl -fsSL -o "${out}" "${url}"; then
    printf 'ERROR: download failed: %s\n' "${url}" >&2
    return 1
  fi
  if command -v sha256sum >/dev/null; then
    actual=$(sha256sum "${out}" | awk '{ print $1 }')
  else
    actual=$(shasum -a 256 "${out}" | awk '{ print $1 }')
  fi
  if [[ "${actual}" != "${expected}" ]]; then
    printf 'ERROR: checksum mismatch for %s: expected %s, got %s\n' \
      "$(basename "${out}")" "${expected}" "${actual}" >&2
    return 1
  fi
}

# export_github_env <name> <value>: make <name> available to later steps of a
# GitHub Actions job; a no-op elsewhere.
export_github_env() {
  if [[ -n "${GITHUB_ENV-}" ]]; then
    echo "$1=$2" >>"${GITHUB_ENV}"
  fi
}
