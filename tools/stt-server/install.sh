#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Part of Voice Command AI (VCA). See the LICENSE file in the repository root.
#
# Installs the reference speech-to-text server into one folder.
#
#   ./install.sh [--dir DIR] [--cpu] [--cuda-libs] [--ct2-wheel PATH] [--cron]
#
#   --dir DIR         install folder (default: ~/stt-server)
#   --cpu             CPU only: do not check for a GPU
#   --cuda-libs       also pip-install the CUDA 12 cuBLAS and cuDNN 9 runtime
#                     libraries into the venv (x86_64 Linux without a system CUDA)
#   --ct2-wheel PATH  install this CTranslate2 wheel first (for example a CUDA
#                     build you compiled yourself for ARM64/aarch64)
#   --cron            add a user crontab "@reboot" entry (no root needed); prefer
#                     the systemd unit when you have root
#
# Layout (everything stays inside DIR, so moving the server means installing on
# the new machine and copying data/tokens.json):
#   DIR/app/vca_stt_server   the code        DIR/venv   Python environment
#   DIR/data                 tokens, logs    DIR/hf     model cache
#   DIR/stt.env              settings        DIR/run.sh start script
set -euo pipefail

DIR="$HOME/stt-server"; WHEEL=""; CPU=0; CUDA_LIBS=0; CRON=0
while [ $# -gt 0 ]; do
  case "$1" in
    --dir) DIR="$2"; shift 2 ;;
    --ct2-wheel) WHEEL="$2"; shift 2 ;;
    --cpu) CPU=1; shift ;;
    --cuda-libs) CUDA_LIBS=1; shift ;;
    --cron) CRON=1; shift ;;
    -h|--help) sed -n '5,22p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

SRC="$(cd "$(dirname "$0")" && pwd)"
PY="${PYTHON:-python3}"
"$PY" -c 'import sys; sys.exit(0 if sys.version_info >= (3, 10) else 1)' \
  || { echo "Python 3.10 or newer is required ($PY)" >&2; exit 1; }

umask 077
mkdir -p "$DIR/app" "$DIR/data" "$DIR/hf"
chmod 700 "$DIR"
rm -rf "$DIR/app/vca_stt_server"
cp -R "$SRC/vca_stt_server" "$DIR/app/"
cp "$SRC/run.sh" "$SRC/requirements.txt" "$DIR/"
chmod 700 "$DIR/run.sh"

cd "$DIR"
[ -d venv ] || "$PY" -m venv venv
venv/bin/pip install -q --upgrade pip
if [ -n "$WHEEL" ]; then
  venv/bin/pip install -q "$WHEEL"
fi
venv/bin/pip install -q -r requirements.txt
if [ "$CUDA_LIBS" = 1 ]; then
  venv/bin/pip install -q nvidia-cublas-cu12 "nvidia-cudnn-cu12==9.*"
fi

if [ ! -f stt.env ]; then
  cat > stt.env <<'ENV'
# Settings for the speech-to-text server (read by run.sh and the systemd unit).
VCA_STT_HOST=127.0.0.1
VCA_STT_PORT=8770
# Empty: large-v3-turbo on a GPU, small on a CPU.
VCA_STT_MODEL=
# auto, cuda or cpu; empty compute type: float16 on a GPU, int8 on a CPU.
VCA_STT_DEVICE=auto
VCA_STT_COMPUTE=
VCA_STT_RATE_PER_MIN=120
VCA_STT_MAX_QUEUE=4
VCA_STT_MAX_AUDIO_SECONDS=600
ENV
fi
[ -f data/tokens.json ] || { echo '{}' > data/tokens.json; chmod 600 data/tokens.json; }

ARCH="$(uname -m)"
if [ "$CPU" = 0 ]; then
  # run.sh puts pip-installed NVIDIA libraries on the library path; do the same here.
  NV_LIBS="$(ls -d venv/lib/python3*/site-packages/nvidia/*/lib 2>/dev/null | tr '\n' ':' || true)"
  COUNT="$(LD_LIBRARY_PATH="${NV_LIBS}${LD_LIBRARY_PATH:-}" venv/bin/python -c \
    'import ctranslate2; print(ctranslate2.get_cuda_device_count())' 2>/dev/null || echo 0)"
  if [ "$COUNT" = 0 ]; then
    echo "note: CTranslate2 sees no CUDA GPU; the server will run on the CPU."
    case "$ARCH" in
      aarch64|arm64)
        [ "$(uname -s)" = Linux ] && echo "      On ARM64 Linux, PyPI's CTranslate2 is CPU-only: build it with CUDA from source and pass --ct2-wheel." ;;
    esac
  else
    echo "CTranslate2 sees $COUNT CUDA device(s)."
  fi
fi

if [ "$CRON" = 1 ]; then
  ( crontab -l 2>/dev/null | grep -vF "$DIR/run.sh" ; echo "@reboot $DIR/run.sh --background" ) | crontab -
  echo "added an @reboot crontab entry"
fi

cat <<EOF
Installed in $DIR.
  Add a user:  cd $DIR/app && VCA_STT_HOME=$DIR/data ../venv/bin/python -m vca_stt_server.tokens add alice
  Start:       $DIR/run.sh              (foreground; --background to detach)
  Check:       curl http://127.0.0.1:8770/health
The model downloads into $DIR/hf on the first start.
EOF
