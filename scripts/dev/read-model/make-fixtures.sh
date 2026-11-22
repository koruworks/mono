#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0

# Generate the synthetic OCR fixtures (pictures and PDFs with known text,
# encrypted and damaged PDFs) into $READ_MODEL_OUT/fixtures (default
# <repo>/target/ocr-measure/fixtures), in a throwaway venv of its own
# ($READ_MODEL_OUT/venv-fixtures) installed from fixtures.lock.
#
#   bash scripts/dev/read-model/make-fixtures.sh [--clean]

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${HERE}/../../.." && pwd)"
OUT="${READ_MODEL_OUT:-${ROOT}/target/ocr-measure}"
VENV="${OUT}/venv-fixtures"
DEST="${OUT}/fixtures"

for arg in "$@"; do
  case "${arg}" in
    --clean) rm -rf "${VENV}" "${DEST}" ;;
    *) echo "usage: $0 [--clean]" >&2; exit 2 ;;
  esac
done

if [ -z "${PYTHON:-}" ]; then
  case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) PYTHON="py -3.12" ;;
    *) PYTHON="python3.12" ;;
  esac
fi
if [ ! -d "${VENV}" ]; then
  ${PYTHON} -m venv "${VENV}"
fi
if [ -d "${VENV}/Scripts" ]; then VPY="${VENV}/Scripts/python.exe"; else VPY="${VENV}/bin/python"; fi

PIP_DISABLE_PIP_VERSION_CHECK=1 "${VPY}" -I -m pip install --quiet \
  --require-hashes --no-deps --only-binary :all: -r "${HERE}/fixtures.lock"

echo "[fixtures] writing ${DEST}"
"${VPY}" -I "${HERE}/make_fixtures.py" "${DEST}"
