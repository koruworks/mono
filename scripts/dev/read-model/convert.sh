#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0

# Convert PaddleOCR's PP-OCRv6 small text detector and recognizer to ONNX with
# paddle2onnx, from the official weights, reproducibly.
#
#   bash scripts/dev/read-model/convert.sh            convert (reuses venv/weights)
#   bash scripts/dev/read-model/convert.sh --clean    delete venv, weights and
#                                                     outputs first, then convert
#
# Everything it writes goes under $READ_MODEL_OUT (default
# <repo>/target/ocr-measure), which is ignored by git:
#
#   venv/      throwaway Python environment, from requirements.lock only
#   weights/   the official Paddle inference files, verified by SHA-256
#   models/    det.onnx, rec.onnx, rec_dict.txt and SHA256SUMS
#
# The system Python's site-packages are never touched: the venv is created
# with `python -m venv` and every install goes into it with --require-hashes.
#
# Needs: Python 3.12 (`py -3.12` on Windows, else `python3.12`; override with
# PYTHON=...), curl, sha256sum or shasum. The lock is for Windows x64.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${HERE}/../../.." && pwd)"
OUT="${READ_MODEL_OUT:-${ROOT}/target/ocr-measure}"
VENV="${OUT}/venv"
WEIGHTS="${OUT}/weights"
MODELS="${OUT}/models"

# The official weights: PaddlePaddle's own Hugging Face organisation, at a
# pinned commit of each repository. The .pdiparams digests are the LFS object
# ids Hugging Face publishes for those files; the .json and .yml digests were
# taken by downloading them at the same commit. The same three files are in
# PaddleX's bundles (paddle-model-ecology.bj.bcebos.com/paddlex/
# official_inference_model/paddle3.0.0/PP-OCRv6_small_{det,rec}_infer.tar).
#
# model<TAB>repository<TAB>commit<TAB>file<TAB>bytes<TAB>sha256
WEIGHT_FILES=(
  "det	PaddlePaddle/PP-OCRv6_small_det	106c97591b235f607453300d9fc8c1cad1b25488	inference.json	229307	89240f689a4a77aad75ef55a8df0a15c8e1d4980a327d17e58f24bbadde5aeab"
  "det	PaddlePaddle/PP-OCRv6_small_det	106c97591b235f607453300d9fc8c1cad1b25488	inference.pdiparams	9817888	5043d4ccc8d63402ccea8feefcee4db57077431a873e78d2191836a178a492da"
  "det	PaddlePaddle/PP-OCRv6_small_det	106c97591b235f607453300d9fc8c1cad1b25488	inference.yml	885	193f435274bf9f0b5f71a929bbfbcf148282df7e633b34e7c373e8f44741b516"
  "rec	PaddlePaddle/PP-OCRv6_small_rec	bd619643acac4b9650c040234da8d944476ee3f1	inference.json	208004	f0bf53c853937a917affdd74467472167727f8ab0f0f7bded01c4a16c27e46e6"
  "rec	PaddlePaddle/PP-OCRv6_small_rec	bd619643acac4b9650c040234da8d944476ee3f1	inference.pdiparams	21074618	406e1e689c9a7fbb04178007a3fe10cf852afe7bf8bb3bc6dbb9d532b13bd907"
  "rec	PaddlePaddle/PP-OCRv6_small_rec	bd619643acac4b9650c040234da8d944476ee3f1	inference.yml	150579	ab078671bb49f06228eadccd34f1bb501e157f7a047095ffb943ba81512c77d1"
)

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

size_of() {
  stat -f%z "$1" 2>/dev/null || stat -c%s "$1"
}

CLEAN=0
for arg in "$@"; do
  case "${arg}" in
    --clean) CLEAN=1 ;;
    *) echo "usage: $0 [--clean]" >&2; exit 2 ;;
  esac
done

if [ "${CLEAN}" = "1" ]; then
  echo "[read-model] removing ${VENV}, ${WEIGHTS} and ${MODELS}"
  rm -rf "${VENV}" "${WEIGHTS}" "${MODELS}"
fi
mkdir -p "${OUT}" "${WEIGHTS}" "${MODELS}"

# --- 1. the Python environment --------------------------------------------------

if [ -z "${PYTHON:-}" ]; then
  case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) PYTHON="py -3.12" ;;
    *) PYTHON="python3.12" ;;
  esac
fi

if [ -d "${VENV}/Scripts" ]; then VBIN="${VENV}/Scripts"; else VBIN="${VENV}/bin"; fi
if [ ! -x "${VBIN}/python" ] && [ ! -x "${VBIN}/python.exe" ]; then
  echo "[read-model] creating ${VENV} with ${PYTHON}"
  ${PYTHON} -m venv "${VENV}"
  if [ -d "${VENV}/Scripts" ]; then VBIN="${VENV}/Scripts"; else VBIN="${VENV}/bin"; fi
fi
VPY="${VBIN}/python"
[ -x "${VPY}.exe" ] && VPY="${VPY}.exe"

echo "[read-model] installing the pinned tools (requirements.lock, hashes required)"
PIP_DISABLE_PIP_VERSION_CHECK=1 "${VPY}" -I -m pip install --quiet \
  --require-hashes --no-deps --only-binary :all: -r "${HERE}/requirements.lock"
"${VPY}" -I -m pip check

# --- 2. the official weights -----------------------------------------------------

for row in "${WEIGHT_FILES[@]}"; do
  IFS=$'\t' read -r model repo commit file bytes want <<<"${row}"
  dir="${WEIGHTS}/${model}"
  final="${dir}/${file}"
  mkdir -p "${dir}"
  if [ ! -f "${final}" ]; then
    url="https://huggingface.co/${repo}/resolve/${commit}/${file}"
    echo "[read-model] fetching ${url}"
    curl -fSL --retry 3 -o "${final}.part" "${url}"
    mv "${final}.part" "${final}.unverified"
    final_tmp="${final}.unverified"
  else
    final_tmp="${final}"
  fi
  have_size="$(size_of "${final_tmp}")"
  have="$(sha256_of "${final_tmp}")"
  if [ "${have_size}" != "${bytes}" ] || [ "${have}" != "${want}" ]; then
    echo "[read-model] ${model}/${file} does not match its pin" >&2
    echo "[read-model]   want ${bytes} bytes sha256 ${want}" >&2
    echo "[read-model]   have ${have_size} bytes sha256 ${have}" >&2
    rm -f "${final_tmp}"
    exit 1
  fi
  [ "${final_tmp}" != "${final}" ] && mv "${final_tmp}" "${final}"
  echo "[read-model] verified ${model}/${file}  ${have}"
done

# --- 3. the conversion -----------------------------------------------------------

# paddle2onnx's defaults, written out: opset 9 raised automatically to the
# lowest opset the graph needs (PaddlePaddle's own ONNX exports of these two
# models come out at opset 14 for the detector and 11 for the recognizer), the
# ONNX checker on, and polygraphy's constant folding.
export POLYGRAPHY_AUTOINSTALL_DEPS=0
export PYTHONHASHSEED=0
for model in det rec; do
  echo "[read-model] converting ${model}"
  "${VPY}" -I -m paddle2onnx.command \
    --model_dir "${WEIGHTS}/${model}" \
    --model_filename inference.json \
    --params_filename inference.pdiparams \
    --save_file "${MODELS}/${model}.onnx" \
    --opset_version 9 \
    --enable_auto_update_opset True \
    --enable_onnx_checker True \
    --optimize_tool polygraphy
done

"${VPY}" -I "${HERE}/extract_dict.py" "${WEIGHTS}/rec/inference.yml" "${MODELS}/rec_dict.txt"

# --- 4. the digests --------------------------------------------------------------

echo
echo "[read-model] outputs (sha256, bytes, file):"
: > "${MODELS}/SHA256SUMS"
for f in det.onnx rec.onnx rec_dict.txt; do
  h="$(sha256_of "${MODELS}/${f}")"
  s="$(size_of "${MODELS}/${f}")"
  printf '%s  %s\n' "${h}" "${f}" >> "${MODELS}/SHA256SUMS"
  printf '  %s  %10s  %s\n' "${h}" "${s}" "${f}"
done
echo "[read-model] written to ${MODELS}"
