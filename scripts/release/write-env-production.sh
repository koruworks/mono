#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# Write .env.production for a stable release build (docs/features/
# github-release/STABLE.md, S8) from the job's environment, which the vault
# restores into GitHub's `stable` environment. These are koru_env::BAKED,
# what the stable on the update origin always carried: Koru Cloud, Google
# Docs import, the support and feedback links.
#
# An empty value fails here, by name, rather than as a build without the
# feature it serves. The file is ignored by git, so the tree stays clean for
# koru-release's check.

set -euo pipefail

names=(
  KORU_PUBLIC_API_ORIGIN
  KORU_PUBLIC_API_ISSUER
  KORU_PUBLIC_API_CREDENTIAL_ROLLOUT
  KORU_SUPPORT_URL
  KORU_FEEDBACK_URL
  KORU_GOOGLE_DOCS_CLIENT_ID
  KORU_GOOGLE_DOCS_CLIENT_SECRET
)
for name in "${names[@]}"; do
  if [ -z "${!name:-}" ]; then
    echo "::error::$name is empty in this environment: restore it from the vault (github_environment.py --apply)"
    exit 1
  fi
done
(
  umask 077
  for name in "${names[@]}"; do
    printf '%s=%s\n' "$name" "${!name}"
  done > .env.production
)
echo "Wrote .env.production: ${names[*]}"
