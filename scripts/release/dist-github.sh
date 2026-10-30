#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# One platform's `koru-release dist-github`, for .github/workflows/
# release-build.yml: the same flags on Linux, Windows and macOS, for the
# nightly and the stable release. The arguments are how this platform runs
# koru-release:
#
#   bash scripts/release/dist-github.sh ./koru.sh release-tool
#   bash scripts/release/dist-github.sh cargo run --locked -q -p koru-release --
#
# The job's environment says what to build: CHANNEL, BUILD, NOTES,
# DOWNLOAD_BASE and the Minisign pair MINISIGN_KEY and MINISIGN_PASSWORD
# (empty when the key has none); VERSION, UPDATE_BASE and WITH_CLOUD when
# they apply. The files land in dist/<channel>/.

set -euo pipefail

key="$RUNNER_TEMP/minisign.key"
(umask 077 && printf '%s\n' "$MINISIGN_KEY" > "$key")
trap 'rm -f "$key"' EXIT

args=(dist-github "$CHANNEL"
  --build "$BUILD"
  --notes "$NOTES"
  --key "$key" --key-password-stdin
  --download-base "$DOWNLOAD_BASE"
  --out "dist/$CHANNEL")
if [ -n "${VERSION:-}" ]; then args+=(--version "$VERSION"); fi
if [ -n "${UPDATE_BASE:-}" ]; then args+=(--update-base "$UPDATE_BASE"); fi
if [ "${WITH_CLOUD:-false}" = true ]; then args+=(--with-cloud); fi

printf '%s' "${MINISIGN_PASSWORD:-}" | "$@" "${args[@]}"
