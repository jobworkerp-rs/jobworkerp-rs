#!/usr/bin/env bash
# Usage: verify-server-artifact.sh <binary> <linux-x86_64|linux-aarch64|darwin-arm64> <expected-version>
#
# Smoke check of a built binary, run by the public release workflow on the
# runner that built it, before anything is published. It checks what makes a
# release unusable on the user's machine: file format and architecture; that
# Linux builds link glibc dynamically (the PLUGIN runner loads plugins with
# dlopen, which a static binary cannot) but only the C and C++ runtime
# libraries, and no newer glibc / libstdc++ than the documented floor; that
# macOS builds link only system libraries, carry a valid signature and do not
# demand a newer macOS than MACOSX_DEPLOYMENT_TARGET; and that the binary runs
# and reports the release version as "<file name> <version>".
#
# Exit status: 1 when the artifact fails a check, 2 on usage errors.
# Kept compatible with the bash 3.2 shipped with macOS.
set -euo pipefail

binary=${1-}
platform=${2-}
expected_version=${3-}

die() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

usage_error() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 2
}

# True when dotted version $1 is greater than $2. Compared field by field in
# awk because `sort -V` is not available on every macOS release.
version_gt() {
  awk -v a="$1" -v b="$2" 'BEGIN {
    na = split(a, x, "."); nb = split(b, y, "."); n = (na > nb) ? na : nb
    for (i = 1; i <= n; i++) {
      if ((x[i] + 0) > (y[i] + 0)) exit 0
      if ((x[i] + 0) < (y[i] + 0)) exit 1
    }
    exit 1
  }'
}

# The oldest runtime the Linux release supports: Ubuntu 24.04 (glibc 2.39,
# libstdc++ of GCC 14). README.md states the same requirement.
GLIBC_FLOOR=2.39
GLIBCXX_FLOOR=3.4.33

# Shared libraries every glibc system provides; anything else would have to be
# installed by the user.
RUNTIME_LIBRARIES=" libc.so.6 libm.so.6 libpthread.so.0 libdl.so.2 librt.so.1 libgcc_s.so.1 libstdc++.so.6 ld-linux-x86-64.so.2 ld-linux-aarch64.so.1 "

# max_version <prefix> <versions>: the highest <prefix>_x.y.z among <versions>.
max_version() {
  local prefix=$1 versions=$2 max='' version
  for version in ${versions}; do
    [[ "${version}" == "${prefix}"_[0-9]* ]] || continue
    version=${version#"${prefix}"_}
    if [[ -z "${max}" ]] || version_gt "${version}" "${max}"; then
      max=${version}
    fi
  done
  echo "${max}"
}

check_glibc_elf() {
  local kind=$1 arch=$2 needed library versions glibc glibcxx
  [[ "${kind}" == *"ELF 64-bit LSB"*"${arch}"* ]] || die "not a ${arch} ELF executable"
  [[ "${kind}" == *"dynamically linked"* ]] \
    || die "not dynamically linked; plugins are loaded with dlopen, which needs the system glibc"
  needed=$(LC_ALL=C readelf -d "${binary}" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p')
  echo "dynamic dependencies:"
  for library in ${needed}; do
    echo "  ${library}"
    [[ "${RUNTIME_LIBRARIES}" == *" ${library} "* ]] \
      || die "dynamic dependency outside the C and C++ runtimes: ${library}"
  done
  versions=$(LC_ALL=C readelf -V "${binary}" | grep -oE '(GLIBC|GLIBCXX)_[0-9][0-9.]*' || true)
  glibc=$(max_version GLIBC "${versions}")
  glibcxx=$(max_version GLIBCXX "${versions}")
  echo "requires glibc ${glibc:-unknown}, libstdc++ ${glibcxx:-none}"
  [[ -n "${glibc}" ]] || die "no GLIBC symbol version found"
  if version_gt "${glibc}" "${GLIBC_FLOOR}"; then
    die "requires glibc ${glibc}, above the supported ${GLIBC_FLOOR}"
  fi
  if [[ -n "${glibcxx}" ]] && version_gt "${glibcxx}" "${GLIBCXX_FLOOR}"; then
    die "requires libstdc++ GLIBCXX_${glibcxx}, above the supported ${GLIBCXX_FLOOR}"
  fi
}

[[ -n "${binary}" && -n "${platform}" && -n "${expected_version}" ]] \
  || usage_error "usage: $0 <binary> <linux-x86_64|linux-aarch64|darwin-arm64> <expected-version>"
[[ -f "${binary}" ]] || usage_error "no such file: ${binary}"
[[ -x "${binary}" ]] || usage_error "not executable: ${binary}"

kind=$(file -b "${binary}")
echo "file: ${kind}"

case "${platform}" in
  linux-x86_64)
    check_glibc_elf "${kind}" x86-64
    ;;
  linux-aarch64)
    check_glibc_elf "${kind}" "ARM aarch64"
    ;;
  darwin-arm64)
    [[ -n "${MACOSX_DEPLOYMENT_TARGET-}" ]] || usage_error "MACOSX_DEPLOYMENT_TARGET must be set for ${platform}"
    [[ "${kind}" == *"Mach-O 64-bit executable arm64"* ]] || die "not an arm64 Mach-O executable"
    echo "dynamic dependencies:"
    libraries=$(otool -L "${binary}" | tail -n +2 | awk '{ print $1 }')
    for library in ${libraries}; do
      echo "  ${library}"
      case "${library}" in
        /usr/lib/* | /System/Library/*) ;;
        *) die "dynamic dependency outside the macOS system: ${library}" ;;
      esac
    done
    # Captured and fed without a pipe: an awk exiting at the first match
    # would SIGPIPE the writer, which pipefail turns into a silent failure.
    load_commands=$(otool -l "${binary}")
    minos=$(awk '$1 == "minos" { print $2; exit }' <<<"${load_commands}")
    echo "minimum macOS: ${minos:-unknown}"
    [[ -n "${minos}" ]] || die "no LC_BUILD_VERSION minos found"
    if version_gt "${minos}" "${MACOSX_DEPLOYMENT_TARGET}"; then
      die "requires macOS ${minos}, above MACOSX_DEPLOYMENT_TARGET ${MACOSX_DEPLOYMENT_TARGET}"
    fi
    codesign -v --verbose=2 "${binary}" || die "code signature is invalid"
    ;;
  *)
    usage_error "unknown platform: ${platform}"
    ;;
esac

expected="$(basename "${binary}") ${expected_version}"
reported=$("${binary}" --version) || die "--version failed"
echo "version: ${reported}"
[[ "${reported}" == "${expected}" ]] || die "reports '${reported}', expected '${expected}'"

echo "OK: ${binary} (${platform})"
