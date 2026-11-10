#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Part of Voice Command AI (VCA). See the LICENSE file in the repository root.
#
# Starts the speech-to-text server from the folder install.sh created.
#
#   run.sh               run in the foreground (systemd, a terminal)
#   run.sh --background  detach, write server.pid and server.log (cron @reboot)
#   run.sh --stop        stop the detached server started by --background
#   run.sh --status      show whether the detached server is running
#
# Settings come from DIR/stt.env (VCA_STT_* variables), then the environment.
set -euo pipefail
DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$DIR"

running() {
  [ -f server.pid ] && ps -o args= -p "$(cat server.pid)" 2>/dev/null | grep -q "vca_stt_server"
}

case "${1:-}" in
  --status) if running; then echo "running (pid $(cat server.pid))"; else echo "not running"; exit 1; fi; exit 0 ;;
  --stop)   if running; then kill "$(cat server.pid)"; rm -f server.pid; echo "stopped"; else echo "not running"; fi; exit 0 ;;
esac

if [ -f stt.env ]; then
  set -a; . ./stt.env; set +a
fi
export VCA_STT_HOME="${VCA_STT_HOME:-$DIR/data}"
export HF_HOME="${HF_HOME:-$DIR/hf}"
# CUDA libraries installed with pip (install.sh --cuda-libs) live inside the venv.
NV_LIBS="$(ls -d "$DIR"/venv/lib/python3*/site-packages/nvidia/*/lib 2>/dev/null | tr '\n' ':' || true)"
[ -n "$NV_LIBS" ] && export LD_LIBRARY_PATH="${NV_LIBS}${LD_LIBRARY_PATH:-}"
cd "$DIR/app"

if [ "${1:-}" = "--background" ]; then
  if running; then echo "already running (pid $(cat "$DIR/server.pid"))"; exit 0; fi
  nohup "$DIR/venv/bin/python" -m vca_stt_server >> "$DIR/server.log" 2>&1 &
  echo $! > "$DIR/server.pid"
  echo "started (pid $!) on ${VCA_STT_HOST:-127.0.0.1}:${VCA_STT_PORT:-8770}"
else
  exec "$DIR/venv/bin/python" -m vca_stt_server
fi
