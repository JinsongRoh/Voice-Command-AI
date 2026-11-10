# VCA reference speech-to-text server

A small, hardened, OpenAI-compatible Whisper server for Voice Command AI
(VCA), built on [faster-whisper](https://github.com/SYSTRAN/faster-whisper)
and FastAPI. It is meant for one person or a small team who want to run
speech recognition on their own machine (often one with a GPU) and connect
several VCA servers to it.

The full guide (hardware, install on Linux, macOS and Windows, running as a
service, TLS, operations and connecting VCA) is
[docs/whisper-server.md](../../docs/whisper-server.md).

> **Status.** This reference server is published with the next VCA release.
> It ships no model weights: faster-whisper downloads the model you choose
> from Hugging Face on the first start. Check the licence of that model for
> your use.

## What it offers

| Endpoint | Token | What it does |
| --- | --- | --- |
| `POST /v1/audio/transcriptions` | yes | OpenAI-compatible: `file`, `model`, `language`, `prompt`, `response_format` (`json`, `verbose_json`, `text`), `temperature` |
| `GET /v1/models` | yes | Lists `whisper-1` and the real model name (both pick the same model) |
| `GET /health` | no | `{"ok": true}` |
| `GET /` | no | A short status page (turn off with `VCA_STT_LANDING=0`) |

Security built in:

- **Per-user bearer tokens.** Only SHA-256 hashes are stored, in
  `tokens.json` (mode 600). Tokens can expire; removing a user revokes the
  token at once. The file is reloaded on change, so no restart is needed.
- **Every other path needs a token**, unknown ones included (401), so paths
  cannot be probed. The API docs pages are off.
- **Per-IP lockout:** 20 missing or bad tokens within 10 minutes lock that IP
  out for 10 minutes. `/`, `/health` and `/favicon.ico` never count.
- **Per-user rate limit** (120 requests a minute by default; live dictation
  sends about one request a second while someone speaks).
- **Body cap** (25 MiB) checked from `Content-Length` before the body is read
  and again while a chunked body streams in; an **audio-length cap** (600 s).
- **One transcription at a time** with a short queue (4 waiting); more get 503.
- **No content in logs:** the request log holds time, user, client IP, bytes,
  audio seconds, processing time and language. Never audio, prompts or text.
  Errors answer `{"detail": "internal error"}` without a traceback.
- **Loopback by default** (`127.0.0.1:8770`). Put TLS in front with a reverse
  proxy or a tunnel; see the guide.

## Files

| File | Purpose |
| --- | --- |
| `vca_stt_server/app.py` | HTTP API and the security guard |
| `vca_stt_server/transcriber.py` | faster-whisper wrapper |
| `vca_stt_server/tokens.py` | Token store and the `tokens` command |
| `vca_stt_server/config.py` | Settings from `VCA_STT_*` environment variables |
| `install.sh` | Installs into one folder (Linux, macOS) |
| `run.sh` | Starts the server (foreground, or `--background` for cron) |
| `run.ps1` | Starts the server on Windows |
| `vca-stt.service` | Example systemd unit with hardening |
| `tests/` | Tests with a fake transcriber (no model download) |

## Quick start (Linux or macOS)

```sh
cd tools/stt-server
./install.sh --dir ~/stt-server          # add --cuda-libs on x86_64 Linux without system CUDA
cd ~/stt-server/app
VCA_STT_HOME=~/stt-server/data ../venv/bin/python -m vca_stt_server.tokens add alice   # prints the token once
~/stt-server/run.sh
```

Then, from the same machine:

```sh
curl http://127.0.0.1:8770/health
curl -H "Authorization: Bearer $TOKEN" -F file=@sample.wav -F model=whisper-1 \
  http://127.0.0.1:8770/v1/audio/transcriptions
```

## Users

Run these in `DIR/app` with `VCA_STT_HOME=DIR/data` (or set `VCA_STT_TOKENS`
to the file):

```sh
../venv/bin/python -m vca_stt_server.tokens add alice            # create, or rotate an existing user
../venv/bin/python -m vca_stt_server.tokens add bob --days 30    # expires after 30 days
../venv/bin/python -m vca_stt_server.tokens list                 # names and expiry, never tokens
../venv/bin/python -m vca_stt_server.tokens remove alice         # revokes at once
```

Tokens look like `stt_` followed by 43 random URL-safe characters. Each is
printed once; hand it to its user over a private channel.

## Settings

| Variable | Default | Meaning |
| --- | --- | --- |
| `VCA_STT_HOST` | `127.0.0.1` | Listen address. Keep loopback behind a proxy or tunnel |
| `VCA_STT_PORT` | `8770` | Listen port |
| `VCA_STT_HOME` | current folder | Data folder: `tokens.json`, `requests.log` |
| `VCA_STT_TOKENS` | `$VCA_STT_HOME/tokens.json` | Token file |
| `VCA_STT_LOG_FILE` | `$VCA_STT_HOME/requests.log` | Request log (5 MiB x 3 rotation); `-` writes to standard error (journald) |
| `VCA_STT_MODEL` | `large-v3-turbo` (GPU) / `small` (CPU) | A faster-whisper model name, a Hugging Face repo id or a local folder |
| `VCA_STT_DEVICE` | `auto` | `auto`, `cuda` or `cpu` |
| `VCA_STT_COMPUTE` | `float16` (GPU) / `int8` (CPU) | CTranslate2 compute type, such as `int8_float16` |
| `VCA_STT_BEAM_SIZE` | `5` | Lower is faster (`1` is greedy) |
| `VCA_STT_VAD` | `1` | Skip silence with the built-in voice-activity filter |
| `VCA_STT_MODEL_DIR` | Hugging Face cache (`HF_HOME`) | Where models are stored |
| `VCA_STT_MAX_BYTES` | `26214400` | Largest request body (25 MiB) |
| `VCA_STT_MAX_AUDIO_SECONDS` | `600` | Longest audio |
| `VCA_STT_RATE_PER_MIN` | `120` | Requests per user per minute |
| `VCA_STT_MAX_QUEUE` | `4` | Requests waiting behind the running one |
| `VCA_STT_FAIL_LIMIT` | `20` | Failed auths per IP before a lockout |
| `VCA_STT_FAIL_WINDOW` | `600` | ... counted over this many seconds |
| `VCA_STT_LOCKOUT` | `600` | Lockout length in seconds |
| `VCA_STT_TRUSTED_PROXIES` | `127.0.0.1,::1` | Peers whose `X-Real-IP` / `X-Forwarded-For` is trusted |
| `VCA_STT_LANDING` | `1` | Serve the status page at `/` |

`run.sh` and `run.ps1` read these from `stt.env` in the install folder.

## Tests

The tests use a fake transcriber, so they need neither faster-whisper nor a
model:

```sh
cd tools/stt-server
python -m pip install -r requirements-test.txt
python -m pytest -q
```

## Licence

MIT, like the rest of this repository (see `LICENSE` in the repository
root). faster-whisper and CTranslate2 are MIT-licensed; the Whisper models
are published by their authors under their own licences.
