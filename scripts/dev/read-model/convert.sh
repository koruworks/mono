#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0

# Convert PaddleOCR's PP-OCRv6 small text detector and recognizer to ONNX.
#
# The conversion is a command of the release tool (OCR Plan 2.2):
#
#   koru-release read-model convert [--work <dir>] [--clean]
#
# which owns every pin — the official weights' sizes and digests and the
# outputs' in crates/koru-release/src/read_model/table.rs, the tools in
# crates/koru-release/read-model/requirements.lock — and refuses an output that
# differs from the table. This wrapper holds no pin of its own; it keeps Plan
# 0.1's command working with Plan 0.1's output directory:
#
#   bash scripts/dev/read-model/convert.sh [--clean]
#
# writes under $READ_MODEL_OUT (default <repo>/target/ocr-measure): venv/,
# weights/, and models/ with det.onnx, rec.onnx, rec_dict.txt and SHA256SUMS.
# Windows x64 only, for the reason the lock gives.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${HERE}/../../.." && pwd)"
OUT="${READ_MODEL_OUT:-${ROOT}/target/ocr-measure}"

ARGS=()
for arg in "$@"; do
  case "${arg}" in
    --clean) ARGS+=(--clean) ;;
    *) echo "usage: $0 [--clean]" >&2; exit 2 ;;
  esac
done

exec cargo run --release --manifest-path "${ROOT}/Cargo.toml" -p koru-release -- \
  read-model convert --work "${OUT}" "${ARGS[@]}"
