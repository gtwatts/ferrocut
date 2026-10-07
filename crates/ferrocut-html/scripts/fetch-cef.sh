#!/usr/bin/env bash
# Fetch the pinned official CEF prebuilt Linux x64 "minimal" binary distribution
# (BSD-3-Clause; bundles Chromium binaries under their own permissive licenses,
# see third_party/cef/LICENSE.txt and Release/ CREDITS after extraction).
# Lands in crates/ferrocut-html/third_party/ (gitignored). Idempotent.
set -euo pipefail

CEF_VERSION="${CEF_VERSION:-154.0.34+g14c5a08+chromium-154.0.8037.98}"
CEF_SHA1="${CEF_SHA1:-15c91ad767097217b2e82ba8b933fe30b134be69}"
CEF_PLATFORM="linux64"

here="$(cd "$(dirname "$0")/.." && pwd)"
tp="$here/third_party"
mkdir -p "$tp"
name="cef_binary_${CEF_VERSION}_${CEF_PLATFORM}_minimal"
url="https://cef-builds.spotifycdn.com/$(python3 -c 'import sys,urllib.parse;print(urllib.parse.quote(sys.argv[1]))' "$name.tar.bz2")"

if [[ -f "$tp/cef/.ferrocut-version" ]] && [[ "$(cat "$tp/cef/.ferrocut-version")" == "$CEF_VERSION" ]]; then
  echo "CEF $CEF_VERSION already present in $tp/cef"; exit 0
fi

if [[ ! -f "$tp/$name.tar.bz2" ]] || ! echo "$CEF_SHA1  $tp/$name.tar.bz2" | sha1sum -c --quiet - 2>/dev/null; then
  echo "downloading $url"
  curl -fL --retry 3 -o "$tp/$name.tar.bz2.part" "$url"
  mv "$tp/$name.tar.bz2.part" "$tp/$name.tar.bz2"
fi
echo "$CEF_SHA1  $tp/$name.tar.bz2" | sha1sum -c -

rm -rf "$tp/$name" "$tp/cef"
tar -xjf "$tp/$name.tar.bz2" -C "$tp"
ln -sfn "$name" "$tp/cef"
echo "$CEF_VERSION" > "$tp/cef/.ferrocut-version"
echo "CEF $CEF_VERSION ready at $tp/cef"
