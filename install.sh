#!/bin/sh
# Installs the graphersal-server binary from a GitHub Release (Linux, macOS on Apple silicon).
#
#   curl -fsSL https://github.com/graphersal/graphersal-server/releases/latest/download/install.sh | sh
#   curl -fsSL .../install.sh | sh -s -- --version v0.3.1 --dir /usr/local/bin
#
# It picks the archive of this platform, checks it against the release's SHA256SUMS, and puts
# the binary into ~/.local/bin (or --dir). It changes nothing else: no server directory, no
# service, no PATH change.
#
# Options (or environment variables):
#   --version vX.Y.Z   the release to install (GRAPHERSAL_SERVER_VERSION; default: the latest)
#   --dir DIR          where to put the binary (GRAPHERSAL_SERVER_INSTALL_DIR; default ~/.local/bin)
#   --base-url URL     the releases URL (GRAPHERSAL_SERVER_BASE_URL; default
#                      https://github.com/graphersal/graphersal-server/releases); it holds
#                      latest/download/<file> and download/<tag>/<file>, as GitHub does
#   --help             this text

set -eu

REPO_RELEASES="https://github.com/graphersal/graphersal-server/releases"
FROM_SOURCE="cargo install --locked graphersal-server"
version="${GRAPHERSAL_SERVER_VERSION:-}"
dir="${GRAPHERSAL_SERVER_INSTALL_DIR:-}"
base="${GRAPHERSAL_SERVER_BASE_URL:-$REPO_RELEASES}"

say() { printf '%s\n' "$*"; }
fail() {
  printf 'graphersal-server installer: %s\n' "$*" >&2
  exit 1
}

while [ $# -gt 0 ]; do
  case "$1" in
    --version) [ $# -ge 2 ] || fail "--version needs a value (vX.Y.Z)"; version="$2"; shift 2 ;;
    --version=*) version="${1#*=}"; shift ;;
    --dir) [ $# -ge 2 ] || fail "--dir needs a value"; dir="$2"; shift 2 ;;
    --dir=*) dir="${1#*=}"; shift ;;
    --base-url) [ $# -ge 2 ] || fail "--base-url needs a value"; base="$2"; shift 2 ;;
    --base-url=*) base="${1#*=}"; shift ;;
    -h | --help)
      say "Installs graphersal-server from a GitHub Release (Linux, macOS on Apple silicon)."
      say "Options: --version vX.Y.Z (default: latest), --dir DIR (default: ~/.local/bin),"
      say "         --base-url URL (default: $REPO_RELEASES)"
      say "Environment: GRAPHERSAL_SERVER_VERSION, GRAPHERSAL_SERVER_INSTALL_DIR,"
      say "             GRAPHERSAL_SERVER_BASE_URL"
      exit 0
      ;;
    *) fail "unknown option '$1' (try --help)" ;;
  esac
done

if [ -z "$dir" ]; then
  [ -n "${HOME:-}" ] || fail "HOME is not set: give the target directory with --dir"
  dir="$HOME/.local/bin"
fi
base="${base%/}"
case "$version" in
  "" | latest) version="" ;;
  v*) ;;
  *) version="v$version" ;;
esac

# The platform, as the release archives name it: linux-x86_64, linux-aarch64, macos-arm64.
os="$(uname -s)"
machine="$(uname -m)"
case "$os" in
  Darwin) os=macos ;;
  Linux) os=linux ;;
  *) fail "no prebuilt binary for $os: on Windows use install.ps1; elsewhere build from source with Rust 1.88 or newer: $FROM_SOURCE" ;;
esac
case "$machine" in
  x86_64 | amd64) arch=x86_64 ;;
  arm64 | aarch64) arch=arm64 ;;
  *) fail "no prebuilt binary for $os on $machine: build from source with Rust 1.88 or newer: $FROM_SOURCE" ;;
esac
if [ "$os" = macos ] && [ "$arch" = x86_64 ]; then
  # A shell running under Rosetta 2 reports x86_64 on Apple silicon: take the native build.
  if [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
    arch=arm64
  else
    fail "no prebuilt binary for macOS on Intel (x86_64): build from source with Rust 1.88 or newer: $FROM_SOURCE"
  fi
fi
if [ "$os" = linux ] && [ "$arch" = arm64 ]; then
  arch=aarch64
fi
platform="$os-$arch"

# Tools.
if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL --retry 3 -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -q -O "$2" "$1"; }
else
  fail "neither curl nor wget is installed"
fi
if command -v sha256sum >/dev/null 2>&1; then
  sha256() { sha256sum "$1" | cut -d ' ' -f 1; }
elif command -v shasum >/dev/null 2>&1; then
  sha256() { shasum -a 256 "$1" | cut -d ' ' -f 1; }
elif command -v openssl >/dev/null 2>&1; then
  sha256() { openssl dgst -sha256 -r "$1" | cut -d ' ' -f 1; }
else
  fail "no SHA-256 tool (sha256sum, shasum or openssl) to verify the download"
fi
command -v tar >/dev/null 2>&1 || fail "tar is not installed"

tmp="$(mktemp -d 2>/dev/null || mktemp -d -t graphersal-server)"
trap 'rm -rf "$tmp"' EXIT
trap 'exit 1' INT TERM

# SHA256SUMS of the release names the archive (and so the version) and its checksum.
if [ -n "$version" ]; then
  release_url="$base/download/$version"
  what="release $version"
else
  release_url="$base/latest/download"
  what="the latest release"
fi
fetch "$release_url/SHA256SUMS" "$tmp/SHA256SUMS" || fail "cannot download SHA256SUMS of $what from $release_url (no such release, or no network; you can build from source: $FROM_SOURCE)"
line="$(grep -E "^[0-9a-f]{64}  graphersal-server-v[^ ]*-$platform\.tar\.gz\$" "$tmp/SHA256SUMS" | head -n 1 || true)"
[ -n "$line" ] || fail "$what has no archive for $platform"
expected="${line%% *}"
archive="${line#*  }"
found_version="${archive#graphersal-server-}"
found_version="${found_version%-"$platform".tar.gz}"
if [ -n "$version" ] && [ "$found_version" != "$version" ]; then
  fail "SHA256SUMS of $version names $archive"
fi
# Download from the release that SHA256SUMS belongs to, even when "latest" moves meanwhile.
[ -n "$version" ] || release_url="$base/download/$found_version"

say "Downloading graphersal-server $found_version for $platform"
fetch "$release_url/$archive" "$tmp/$archive" || fail "cannot download $release_url/$archive"
actual="$(sha256 "$tmp/$archive")"
[ "$actual" = "$expected" ] || fail "checksum mismatch for $archive: expected $expected, got $actual (the download is damaged or was changed; nothing was installed)"
say "Checksum verified (SHA-256 $expected)"

tar -xzf "$tmp/$archive" -C "$tmp"
binary="$tmp/graphersal-server-$found_version-$platform/graphersal-server"
[ -f "$binary" ] || fail "$archive holds no graphersal-server binary"

mkdir -p "$dir" || fail "cannot create $dir"
upgrade=""
[ -f "$dir/graphersal-server" ] && upgrade=1
# Copy next to the target, then rename: a running binary is replaced, never half-written.
cp "$binary" "$dir/.graphersal-server.new" || fail "cannot write to $dir"
chmod 755 "$dir/.graphersal-server.new"
mv -f "$dir/.graphersal-server.new" "$dir/graphersal-server" || fail "cannot write $dir/graphersal-server"
say "Installed $dir/graphersal-server"
"$dir/graphersal-server" --version || fail "the installed binary does not run on this system"

case ":${PATH:-}:" in
  *":$dir:"*) program="graphersal-server" ;;
  *)
    say ""
    say "$dir is not on your PATH. Add it, for example in your shell's profile:"
    say "  export PATH=\"$dir:\$PATH\""
    program="$dir/graphersal-server"
    ;;
esac
say ""
if [ -n "$upgrade" ]; then
  say "Upgraded. A running server keeps the old version until it is restarted:"
  say "  $program stop --dir ./srv"
  say "  $program start --dir ./srv"
else
  say "Next step: create a server directory (it prints the first admin's token once) and start it:"
  say "  $program init ./srv"
  say "  $program start --dir ./srv"
fi
