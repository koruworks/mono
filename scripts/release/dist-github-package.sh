#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# `koru-release dist-github-package` for .github/workflows/release-build.yml:
# the phones' half of dist-github.sh. The package is already built and signed
# by its own toolchain (Gradle, for the APK); this signs it with the channel's
# Minisign key and writes it, its `.minisig` and the release body into
# dist/<channel>/. The arguments are how this platform runs koru-release:
#
#   bash scripts/release/dist-github-package.sh cargo run --locked -q -p koru-release --
#
# The job's environment says what to write: CHANNEL, TARGET, PACKAGE,
# VERSION, BUILD, NOTES, DOWNLOAD_BASE and the Minisign pair MINISIGN_KEY and
# MINISIGN_PASSWORD (empty when the key has none).

set -euo pipefail

key="$RUNNER_TEMP/minisign.key"
(umask 077 && printf '%s\n' "$MINISIGN_KEY" > "$key")
trap 'rm -f "$key"' EXIT

printf '%s' "${MINISIGN_PASSWORD:-}" | "$@" dist-github-package "$CHANNEL" \
  --target "$TARGET" \
  --package "$PACKAGE" \
  --version "$VERSION" \
  --build "$BUILD" \
  --notes "$NOTES" \
  --key "$key" --key-password-stdin \
  --download-base "$DOWNLOAD_BASE" \
  --out "dist/$CHANNEL"
