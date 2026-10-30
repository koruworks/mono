#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# KORU_MAC_SIGN_COMMAND for the CI nightly (docs/features/linux/NIGHTLY.rev2.md
# §6): Developer ID, the hardened runtime and a secure timestamp, from the
# job's temporary keychain. The workflow sets the command to
#
#   bash scripts/release/mac-sign.sh {file} {entitlements}
#
# koru-release runs it once per helper, then for the share extension, the
# bundle and the disk image (package::mac_sign, mac_dmg). It hands
# `{entitlements}` as "" for what takes none — the updater and the disk image —
# and a plist for the rest. That plist's audio-input entitlement is what lets
# dictation open the microphone under the hardened runtime, which otherwise
# denies it without an error, so it must reach codesign whenever it is given.
#
# MACOS_SIGNING_IDENTITY and KORU_SIGNING_KEYCHAIN come from the job.

set -euo pipefail

file="$1"
entitlements="${2:-}"
: "${MACOS_SIGNING_IDENTITY:?the Developer ID identity to sign with}"
: "${KORU_SIGNING_KEYCHAIN:?the keychain that holds it}"

args=(--force --options runtime --timestamp
  --keychain "$KORU_SIGNING_KEYCHAIN" --sign "$MACOS_SIGNING_IDENTITY")
if [ -n "$entitlements" ]; then
  args+=(--entitlements "$entitlements")
fi
exec codesign "${args[@]}" "$file"
