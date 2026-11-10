# Your own Whisper speech-to-text server

This guide shows how to install, run, secure and operate a self-hosted
Whisper speech-to-text server, and how to connect Voice Command AI (VCA) to
it. It suits a single PC as well as one GPU machine shared by a small team.

VCA talks to any server that offers the OpenAI-compatible
`POST /v1/audio/transcriptions`. Your audio goes to your server and nowhere
else. For a quick local engine on the same PC, see Step 1 of
[speech-engines.md](speech-engines.md); this guide goes further: a hardened
server, users and tokens, TLS, running it as a service and day-to-day
operations.

Contents:

1. [Choosing a setup](#1-choosing-a-setup)
2. [Hardware and models](#2-hardware-and-models)
3. [Install](#3-install)
4. [Run as a service](#4-run-as-a-service)
5. [Users and tokens](#5-users-and-tokens)
6. [Security](#6-security)
7. [Operations](#7-operations)
8. [Connecting VCA](#8-connecting-vca)
9. [Privacy](#9-privacy)

## 1. Choosing a setup

| | Option A: Speaches (Docker) | Option B: VCA reference server |
| --- | --- | --- |
| What | [Speaches](https://github.com/speaches-ai/speaches) (formerly faster-whisper-server), MIT | [`tools/stt-server`](../tools/stt-server/README.md) in this repository, MIT |
| Engine | faster-whisper | faster-whisper |
| Install | One `docker run` | `install.sh` into one folder, or a few `pip` steps on Windows |
| Users | One shared API key (if set) | Per-user tokens with expiry, hashed on disk |
| Abuse protection | Put it behind a proxy that adds limits | Built in: per-IP lockout, per-user rate limit, body and audio caps, bounded queue |
| Logs | Its own logging | Content-free request log with rotation |
| Best for | Trying it out, one person on one PC | A server reached over a network, several people |

Both work with VCA in the same way: only the base URL, the model name and
the key differ.

### Option A: Speaches with Docker (quickest)

On a CPU:

```sh
docker run -d --name speaches --restart unless-stopped \
  -p 127.0.0.1:8000:8000 \
  -v speaches-cache:/home/ubuntu/.cache/huggingface/hub \
  ghcr.io/speaches-ai/speaches:latest-cpu
```

With an NVIDIA GPU (needs the NVIDIA Container Toolkit):

```sh
docker run -d --name speaches --restart unless-stopped --gpus all \
  -p 127.0.0.1:8000:8000 \
  -v speaches-cache:/home/ubuntu/.cache/huggingface/hub \
  ghcr.io/speaches-ai/speaches:latest-cuda
```

Download a model once and check it (the exact steps can change between
Speaches versions; see its documentation):

```sh
curl -X POST http://127.0.0.1:8000/v1/models/Systran/faster-whisper-small
curl http://127.0.0.1:8000/v1/models
```

`-p 127.0.0.1:8000:8000` keeps it on this machine. If other machines must
reach it, do not publish the port on all interfaces: set an API key (recent
Speaches versions accept an `API_KEY` setting; check its documentation) and
put TLS in front as in [Security](#6-security).

### Option B: VCA's reference server

A small FastAPI app around faster-whisper with the protections a shared
server needs. The rest of this guide uses it; most of the operations advice
applies to Option A as well.

## 2. Hardware and models

### CPU or GPU

- **CPU only.** Works everywhere, including macOS and ARM64. Use a small model
  with `int8`. Good for transcribing files and for one person dictating
  with short chunks; large models on a CPU are usually too slow for live
  dictation.
- **NVIDIA GPU (x86_64 Linux or Windows).** The usual choice for live
  dictation or several users. PyPI's CTranslate2 (the engine inside
  faster-whisper) includes CUDA support on x86_64. Current releases need
  **CUDA 12 and cuDNN 9** libraries: either a system install, or the pip
  packages `nvidia-cublas-cu12` and `nvidia-cudnn-cu12` (the installer's
  `--cuda-libs` option).
- **ARM64 Linux with an NVIDIA GPU.** PyPI's CTranslate2 wheels for aarch64
  are **CPU-only**. For the GPU, build CTranslate2 from source with CUDA
  (see [ARM64: build CTranslate2 with CUDA](#arm64-build-ctranslate2-with-cuda))
  and pass the wheel to the installer.
- **macOS.** CTranslate2 has no Metal backend, so faster-whisper runs on the
  CPU (Apple Silicon CPUs are reasonably fast with `int8`). If you need GPU
  speed on a Mac, consider a whisper.cpp server built with Metal that offers
  an OpenAI-compatible endpoint; VCA connects to it the same way.
- **AMD and Intel GPUs** are not supported by CTranslate2; use the CPU or
  another OpenAI-compatible server that supports your GPU.

### Choosing a model

| Model | Download (fp16) | Notes |
| --- | --- | --- |
| `small` | about 0.5 GB | Default on a CPU. Good for clear speech in common languages |
| `medium` | about 1.5 GB | Better accuracy, about twice as slow as `small` |
| `large-v3-turbo` | about 1.6 GB | Default on a GPU. Close to `large-v3` accuracy for most languages and several times faster, because its decoder is much smaller |
| `large-v3` | about 3 GB | Highest accuracy, slowest; for files rather than live dictation |
| `distil-large-v3` | about 1.5 GB | Fast, English only |

Set the model with `VCA_STT_MODEL`: a faster-whisper name like the ones
above, a Hugging Face repository id of a CTranslate2 conversion, or a local
folder.

### Precision (compute type)

- GPU: `float16` (default) is the usual choice. `int8_float16` uses less
  memory with a small accuracy cost.
- CPU: `int8` (default) is much faster than `float32` with little accuracy
  cost.

### Memory

Rough planning figures; measure on your own machine:

- **GPU memory:** plan for about twice the model's file size in `float16`
  (weights plus working memory), less with `int8_float16`. Leave headroom
  if other programs, such as a local LLM, share the GPU.
- **System RAM:** at least the model size plus 2 GB for the process. A CPU
  server running `small` with `int8` is comfortable with 4 GB free.
- **Disk:** the model cache (`DIR/hf`) plus a few MB for logs.

### Measuring speed

Speed depends on the GPU or CPU, the model, the precision, the beam size and
the language. Measure it yourself with a short clip that sounds like real
use:

```sh
curl -s -o /dev/null -w "%{time_total}s\n" \
  -H "Authorization: Bearer $TOKEN" -F file=@clip.wav -F model=whisper-1 \
  http://127.0.0.1:8770/v1/audio/transcriptions
```

Repeat it a few times (the first request after a start is slower). As a
guide for **live dictation**, a 2 to 15 second chunk should come back well
under a second; if not, choose a smaller model, `VCA_STT_BEAM_SIZE=1`, or a
GPU. For **file transcription** a slower model is fine as long as it is
faster than real time.

## 3. Install

You need Python 3.10 or newer and, for a GPU, the NVIDIA driver. The
installer puts everything in one folder (`--dir`, default `~/stt-server`):

```text
DIR/app/vca_stt_server   the code
DIR/venv                 Python environment
DIR/data                 tokens.json and requests.log
DIR/hf                   model cache
DIR/stt.env              settings (VCA_STT_* variables)
DIR/run.sh               start script
```

### Linux (x86_64)

```sh
sudo apt install python3 python3-venv git    # or your distribution's equivalent
git clone <URL of this repository> Voice-Command-AI
cd Voice-Command-AI/tools/stt-server
./install.sh --dir ~/stt-server               # add --cuda-libs if CUDA 12 / cuDNN 9 are not installed system-wide
```

The installer reports whether CTranslate2 sees a CUDA GPU. Then add a user
and start it in the foreground to watch the first model download:

```sh
cd ~/stt-server/app
VCA_STT_HOME=~/stt-server/data ../venv/bin/python -m vca_stt_server.tokens add alice
~/stt-server/run.sh
```

In a second terminal:

```sh
curl http://127.0.0.1:8770/health                         # {"ok":true}
curl -H "Authorization: Bearer stt_..." http://127.0.0.1:8770/v1/models
```

For a service, install it as its own user under `/opt/vca-stt` instead; see
[Run as a service](#4-run-as-a-service).

### ARM64: build CTranslate2 with CUDA

On ARM64 Linux with an NVIDIA GPU, build a CUDA-enabled CTranslate2 wheel
first. Follow the "Install from sources" section of the
[CTranslate2 documentation](https://opennmt.net/CTranslate2/installation.html);
in outline:

```sh
sudo apt install build-essential cmake git    # plus the CUDA toolkit and cuDNN for your GPU
git clone --recursive https://github.com/OpenNMT/CTranslate2.git
cd CTranslate2 && git checkout <the release faster-whisper expects>
mkdir build && cd build
cmake .. -DCMAKE_BUILD_TYPE=Release -DWITH_CUDA=ON -DWITH_CUDNN=ON \
  -DWITH_MKL=OFF -DOPENMP_RUNTIME=COMP -DCUDA_ARCH_LIST="<your GPU's compute capability>"
make -j"$(nproc)" && sudo make install && sudo ldconfig
cd ../python
pip install -r install_requirements.txt
python setup.py bdist_wheel                     # the wheel lands in dist/
```

Then install with it:

```sh
./install.sh --dir ~/stt-server --ct2-wheel /path/to/ctranslate2-*.whl
```

Notes:

- If your GPU is newer than the CTranslate2 release, its compute capability
  may be missing from the default architecture list; set it explicitly in
  `CUDA_ARCH_LIST` and use a CUDA toolkit that knows that GPU.
- The wheel links to the `libctranslate2` you installed with `make install`;
  keep that library on the machine (or on `LD_LIBRARY_PATH`) and keep a copy
  of the wheel with your backups so you can reinstall without rebuilding.
- `pip install -r requirements.txt` will not replace a CTranslate2 you
  already installed, as long as its version satisfies faster-whisper.

### macOS

```sh
brew install python@3.12 git
cd Voice-Command-AI/tools/stt-server
PYTHON=python3.12 ./install.sh --dir ~/stt-server --cpu
```

The server runs on the CPU (see above); `small` with `int8` is the default.
To start it at login, create a LaunchAgent, for example
`~/Library/LaunchAgents/com.example.vca-stt.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.example.vca-stt</string>
  <key>ProgramArguments</key><array><string>/Users/YOU/stt-server/run.sh</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardErrorPath</key><string>/Users/YOU/stt-server/server.log</string>
</dict></plist>
```

```sh
launchctl load ~/Library/LaunchAgents/com.example.vca-stt.plist
```

### Windows with WSL 2 (recommended for a GPU)

WSL 2 with a current NVIDIA Windows driver gives Linux programs CUDA access.

1. `wsl --install -d Ubuntu`, then open Ubuntu.
2. Follow the Linux steps above inside Ubuntu (use `--cuda-libs` unless you
   installed the CUDA toolkit for WSL).
3. WSL forwards `127.0.0.1` ports to Windows by default, so the VCA app on
   the same PC reaches `http://127.0.0.1:8770/v1`.

To start it automatically, enable systemd in WSL (`/etc/wsl.conf`:
`[boot]` then `systemd=true`), install the systemd unit as below, and keep
WSL running with a Task Scheduler task at logon that runs
`wsl.exe -d Ubuntu --exec /bin/sleep infinity`.

### Windows (native)

faster-whisper also runs natively on Windows, on the CPU or an NVIDIA GPU.
In PowerShell, with Python 3.10 or newer from python.org:

```powershell
$Dir = "C:\stt-server"
New-Item -ItemType Directory -Force "$Dir\app", "$Dir\data", "$Dir\hf" | Out-Null
Copy-Item -Recurse -Force tools\stt-server\vca_stt_server "$Dir\app\"
Copy-Item tools\stt-server\run.ps1, tools\stt-server\requirements.txt $Dir
py -3.12 -m venv "$Dir\venv"
& "$Dir\venv\Scripts\pip.exe" install -r "$Dir\requirements.txt"
# NVIDIA GPU: the CUDA 12 / cuDNN 9 libraries as pip packages (run.ps1 adds their DLLs to PATH)
& "$Dir\venv\Scripts\pip.exe" install nvidia-cublas-cu12 "nvidia-cudnn-cu12==9.*"
Set-Content "$Dir\stt.env" "VCA_STT_HOST=127.0.0.1`nVCA_STT_PORT=8770"
```

Add a user and start it:

```powershell
cd C:\stt-server\app
$env:VCA_STT_HOME = "C:\stt-server\data"
..\venv\Scripts\python.exe -m vca_stt_server.tokens add alice
powershell -ExecutionPolicy Bypass -File C:\stt-server\run.ps1
```

Restrict `C:\stt-server\data` to the account that runs the server (folder
Properties > Security), because Windows ignores the Unix file mode the
token tool sets.

## 4. Run as a service

### Linux: systemd (recommended)

The example unit [`vca-stt.service`](../tools/stt-server/vca-stt.service)
runs the server as its own unprivileged user with systemd hardening: a
read-only system, write access only to `data` and `hf`, no home
directories, no capabilities, private `/tmp`, restricted namespaces and
address families. `PrivateDevices` stays off because the GPU needs
`/dev/nvidia*`; turn it on for a CPU-only server.

```sh
sudo useradd --system --home-dir /opt/vca-stt --shell /usr/sbin/nologin vca-stt
sudo install -d -o vca-stt -g vca-stt -m 700 /opt/vca-stt
sudo cp -r tools/stt-server /tmp/stt-src && sudo chown -R vca-stt: /tmp/stt-src
sudo -u vca-stt /tmp/stt-src/install.sh --dir /opt/vca-stt     # add --cuda-libs or --ct2-wheel as needed
sudo rm -rf /tmp/stt-src
sudo cp tools/stt-server/vca-stt.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now vca-stt
systemctl status vca-stt
journalctl -u vca-stt -f
```

If the GPU device files are not accessible to every user on your system,
add the service user to the group that owns them (often `video` or
`render`): `sudo usermod -aG video vca-stt`.

### Linux or macOS without root: `@reboot` cron

When you cannot install a systemd unit, `install.sh --cron` adds a user
crontab entry:

```text
@reboot /home/YOU/stt-server/run.sh --background
```

`run.sh --background` writes `server.pid` and appends to `server.log`;
`run.sh --status` and `run.sh --stop` manage it. Cron does not restart the
server if it crashes, so prefer systemd when you can.

### Windows: Task Scheduler

Create a task that starts at boot under a dedicated local account (avoid
running the server as SYSTEM or an administrator):

```powershell
schtasks /Create /TN "VCA speech-to-text server" /SC ONSTART /RU "stt-svc" /RP * `
  /TR "powershell.exe -NoProfile -ExecutionPolicy Bypass -File C:\stt-server\run.ps1"
```

`/RP *` prompts for that account's password. On a personal PC, `/SC ONLOGON`
with your own account is simpler. In the task's properties, untick "Stop the
task if it runs longer than 3 days" and set "If the task fails, restart
every 1 minute".

## 5. Users and tokens

Every API request needs `Authorization: Bearer <token>`. Each person gets
their own token, so one can be revoked without affecting the others. Run
the token tool from `DIR/app` with the data folder set:

```sh
cd /opt/vca-stt/app
export VCA_STT_HOME=/opt/vca-stt/data       # or VCA_STT_TOKENS=/path/to/tokens.json

sudo -u vca-stt -E ../venv/bin/python -m vca_stt_server.tokens add alice           # create
sudo -u vca-stt -E ../venv/bin/python -m vca_stt_server.tokens add guest --days 7  # expires in 7 days
sudo -u vca-stt -E ../venv/bin/python -m vca_stt_server.tokens list                # names and expiry only
sudo -u vca-stt -E ../venv/bin/python -m vca_stt_server.tokens add alice           # rotate: the old token stops working
sudo -u vca-stt -E ../venv/bin/python -m vca_stt_server.tokens remove guest        # revoke
```

(Drop `sudo -u vca-stt -E` for a per-user install in your home folder.)

- A token is `stt_` plus 43 random URL-safe characters (32 bytes from the
  operating system's secure random source). It is printed **once**; give it
  to its user over a private channel and do not keep a copy.
- `tokens.json` holds only SHA-256 hashes, with an optional expiry, and is
  written with mode 600. The server compares in constant time against every
  entry and reloads the file when it changes: no restart is needed.
- An expired token is refused like an unknown one. `list` shows `expired`
  for it; remove it or rotate it.

## 6. Security

### Bind to loopback, put TLS in front

The server listens on `127.0.0.1:8770` by default and speaks plain HTTP.
**Do not expose that port directly.** Reach it through one of these:

**A reverse proxy with TLS**, for example [Caddy](https://caddyserver.com),
which obtains certificates automatically:

```caddyfile
stt.example.com {
    request_body {
        max_size 26MB
    }
    reverse_proxy 127.0.0.1:8770 {
        header_up X-Real-IP {remote_host}
    }
}
```

nginx works too (`proxy_pass http://127.0.0.1:8770;`,
`proxy_set_header X-Real-IP $remote_addr;`, `client_max_body_size 26m;`).
Leave the proxy's access log off, or make sure it does not record request
bodies.

**A tunnel**, when the machine has no public address or you do not want to
open ports:

- **Tailscale:** `tailscale serve --bg 8770` publishes it with HTTPS to the
  devices on your tailnet only (`https://<machine>.<tailnet>.ts.net/v1`).
- **Cloudflare Tunnel:** a named tunnel with an ingress rule
  `service: http://127.0.0.1:8770` for `stt.example.com`.
- **DNSPilot** ([dnspilot.kr](https://dnspilot.kr)): create a tunnel to
  `127.0.0.1:8770` in the DNSPilot Agent and use the HTTPS address it gives.

The tunnel agent and the proxy connect from loopback, so the server trusts
their `X-Real-IP` (or the right-most `X-Forwarded-For` entry) as the client
address. If your proxy runs on another machine, add its address to
`VCA_STT_TRUSTED_PROXIES`. Never add addresses of untrusted clients there:
they could then pick any IP they like for the lockout.

If the network between VCA and this server is only your own LAN or tailnet,
plain HTTP to a private address works, but TLS is still recommended because
the token travels with every request.

### Lockout

- An IP that sends 20 missing or wrong tokens within 10 minutes is refused with
  429 for 10 minutes (`VCA_STT_FAIL_LIMIT`, `VCA_STT_FAIL_WINDOW`,
  `VCA_STT_LOCKOUT`). Even a valid token is refused from a locked IP.
- `GET`/`HEAD` on `/`, `/health` and `/favicon.ico` need no token and never
  count, so uptime monitors and browsers opening the address do not lock
  anyone out.
- **Shared NAT:** people behind the same office or mobile network share one
  public IP, so one misconfigured client can lock out everyone there for 10
  minutes. Fix that client's token; restarting the server clears all
  lockouts (they are kept in memory).
- **No client address from the tunnel:** if a tunnel or proxy does not pass
  the client address, every request appears to come from `127.0.0.1` and
  the lockout applies to everyone at once. Check `ip=` in `requests.log`
  after a request from outside; if it always shows `127.0.0.1`, configure
  the proxy to send `X-Real-IP`, or raise `VCA_STT_FAIL_LIMIT`.

### Rate limits, caps and queue

| Limit | Default | Answer |
| --- | --- | --- |
| Requests per user per minute | 120 (`VCA_STT_RATE_PER_MIN`) | 429 with `Retry-After` |
| Request body | 25 MiB (`VCA_STT_MAX_BYTES`), checked before reading and while streaming | 413 |
| Audio length | 600 s (`VCA_STT_MAX_AUDIO_SECONDS`), checked before the model runs | 413 |
| Concurrent jobs | 1 running, 4 waiting (`VCA_STT_MAX_QUEUE`) | 503 with `Retry-After` |

Live dictation sends roughly one request a second while someone speaks, so
120 a minute leaves room for one active dictation per token.

### Other protections

- Unknown paths also answer 401 without a token, and the interactive API
  docs are off, so the server reveals little to scanners.
- Errors answer `{"detail": "internal error"}` without a traceback.
- The server sends no `Server` header; the status page at `/` sets a strict
  Content Security Policy and can be turned off (`VCA_STT_LANDING=0`).
- Firewall: allow inbound connections only to the proxy or tunnel, never to
  port 8770.

## 7. Operations

### Health checks

- `GET /health` answers `{"ok": true}` without a token. Point an uptime
  monitor at `https://stt.example.com/health`.
- For an end-to-end check, create a `monitor` user and call
  `GET /v1/models` with its token, or transcribe a short clip from a script.
- systemd restarts the server if it exits (`Restart=on-failure`).

### Logs

| Log | Where | What |
| --- | --- | --- |
| Request log | `DIR/data/requests.log`, rotates at 5 MiB, 3 old files | One line per request: time, user, client IP, bytes, audio seconds, processing time, language; lockouts; error type names |
| Service output | `journalctl -u vca-stt` (systemd) or `DIR/server.log` (`--background`) | Start-up, model loading, warnings |

Example request log line:

```text
2026-01-15 09:30:12,345 ok user=alice ip=203.0.113.7 bytes=96044 audio=3.0s took=0.41s lang=en
```

`server.log` does not rotate by itself; with `--background`, add a
logrotate rule, for example `/etc/logrotate.d/vca-stt`:

```text
/home/YOU/stt-server/server.log {
    weekly
    rotate 4
    compress
    copytruncate
    missingok
}
```

Set `VCA_STT_LOG_FILE=-` to send the request log to journald instead of a
file.

### Updates

```sh
cd Voice-Command-AI && git pull
sudo -u vca-stt tools/stt-server/install.sh --dir /opt/vca-stt    # keeps data/ and stt.env
sudo systemctl restart vca-stt
```

To make installs repeatable, pin what works:
`/opt/vca-stt/venv/bin/pip freeze > /opt/vca-stt/requirements.lock`, and
install from that file on other machines. To change the model, edit
`VCA_STT_MODEL` in `stt.env` and restart; the new model downloads on start.

### Backup and moving to another machine

Back up `data/tokens.json` (hashes only, but still keep it private) and
`stt.env`. The model cache (`hf/`) can be downloaded again; keep a copy only
if downloads are slow or the machine is offline. Also keep any
CTranslate2 wheel you built yourself.

To move the server:

1. Install on the new machine (same steps as above).
2. Copy `tokens.json` (keep mode 600 and the service user as owner) and
   `stt.env`.
3. Start the server and check `/health` and a transcription.
4. Point the reverse proxy, tunnel or DNS name at the new machine.

Users keep their tokens and VCA settings; nothing changes on their side if
the address stays the same.

### Monitoring GPU memory

```sh
nvidia-smi --query-gpu=memory.used,memory.total,utilization.gpu --format=csv -l 5
```

- Memory grows after the first requests and then stays allocated; that is
  CTranslate2 caching buffers, not a leak.
- If another program (for example a local LLM) shares the GPU, budget both
  models' memory. Out-of-memory errors show up as 500 answers and
  `error ...` lines in the request log.
- Long uploads take the GPU longer; the queue keeps one job on the GPU at a
  time, so memory use stays predictable.

### Hallucination guard

On silence or noise, Whisper can invent text: repetition loops such as
"O.O.O.O.…" or "thank you thank you …", or a stock phrase such as
"감사합니다." The reference server guards against both:

- It decodes with `condition_on_previous_text=False`, so a hallucination in
  one 30-second window cannot seed a loop in the next.
- It drops a segment whose text compresses too well (`compression_ratio`
  above 2.4, a loop), or whose `no_speech_prob` is above 0.6 while its
  `avg_logprob` is below -1.0 (the model heard no speech and is unsure of
  the words).

- It drops a segment that is, as a whole, a known stock phrase when
  `no_speech_prob` is above 0.5 or `avg_logprob` is below -0.8. The phrases
  are the ones Whisper learned from YouTube: "감사합니다", "시청해 주셔서
  감사합니다", "구독과 좋아요 부탁드립니다", "Thank you.", "Thanks for
  watching!", "Please subscribe", "ご視聴ありがとうございました", "¡Gracias
  por ver!" and similar (the list is in `vca_stt_server/hallucination_phrases.py`;
  matching ignores case, punctuation and spacing). A phrase inside a longer
  segment, or a confident segment, is kept: "도와주셔서 감사합니다 다음 작업
  진행해" is never touched.

VCA also drops such a phrase on its own side: a whole segment that is a stock
phrase with low confidence, or a plain-text chunk that is only that phrase on
short or quiet audio.

VCA also collapses short repetition loops in every transcript, whichever
server produced it, and drops a transcript that is mostly one loop. If you
run another Whisper server (for example Speaches), turn off
`condition_on_previous_text` there if it offers the option.

### Troubleshooting

| Symptom | What to check |
| --- | --- |
| `libcublas.so.12` or `cublas64_12.dll` not found | Install CUDA 12, or `install.sh --cuda-libs` (Linux) / the `nvidia-cublas-cu12` and `nvidia-cudnn-cu12` pip packages (Windows) |
| `libcudnn_ops.so.9` (or similar) not found | Install cuDNN 9 for CUDA 12, or the `nvidia-cudnn-cu12==9.*` pip package |
| Installer says "no CUDA GPU" | Check `nvidia-smi`. On ARM64 Linux, PyPI's CTranslate2 is CPU-only: build it with CUDA |
| `no kernel image is available` or similar CUDA errors | Your CTranslate2 build does not include your GPU's architecture; rebuild with it in `CUDA_ARCH_LIST` |
| First start takes minutes | The model is downloading into `hf/`; watch the service output |
| 401 | Missing, wrong, expired or removed token. VCA needs the token as the engine's API key |
| 429 "too many failed attempts" | That IP is locked out; wait 10 minutes or restart the server. Look for a client with a wrong token |
| 429 "rate limit exceeded" | More than `VCA_STT_RATE_PER_MIN` requests from one user in a minute |
| 503 "busy" | More people transcribing at once than the queue allows: a faster model, a GPU, or a larger `VCA_STT_MAX_QUEUE` |
| 413 | Upload over 25 MiB or audio over 600 s |
| 400 "cannot decode the audio" | An unsupported or broken file. VCA sends WAV, WebM, Ogg or M4A, which faster-whisper decodes |
| Empty text | Silence or very quiet audio; the voice-activity filter removed it (`VCA_STT_VAD=0` to test without it) |
| Wrong language | Set the language in VCA (see below) instead of relying on detection |
| Slow on a GPU | Check `nvidia-smi` shows the process; the service output shows `device=cuda` at start-up |

## 8. Connecting VCA

### In the desktop app

On the PC that runs the VCA server, open **Settings > Speech engines on
this PC** and add a speech recognition engine, as described in Step 4 of
[speech-engines.md](speech-engines.md):

| Field | Value |
| --- | --- |
| Start from | Local OpenAI-compatible server |
| Base URL | `https://stt.example.com/v1` (or `http://127.0.0.1:8770/v1` on the same machine) |
| Model | `whisper-1` |
| API key | Your personal token (`stt_...`) |

Use the model name `whisper-1`: the server maps it to its real model, and
VCA then asks for `verbose_json`, which carries the detected language and
per-segment confidence. Click **Test** (VCA sends a one-second tone, so the
text may be empty; "Works" is what matters), tick **Use as the default** if
you want it for every device, **Save**, and restart the VCA server when the
app asks.

The token is stored in the VCA server's credential store on the PC, never
in `config.toml`, and is never shown again.

### In `config.toml`

```toml
[speech]
default_engine = "whisper-server"
language = "en"                        # primary language; omit to auto-detect
# secondary_languages = ["ko"]

[[speech.engines]]
id = "whisper-server"
label = "Whisper server"
kind = "openai_compatible"
base_url = "https://stt.example.com/v1"
model = "whisper-1"
api_key_env = "VCA_STT_TOKEN"          # the token comes from this environment variable
# credential = "team-stt"              # or a stored key shared between profiles
# timeout_secs = 30
```

For the key, either:

- set the environment variable named by `api_key_env` for the VCA server
  process (`VCA_STT_TOKEN=stt_...`), or
- store it on the PC with `vca speech login whisper-server` (it asks for the
  key with hidden input, or reads it from standard input). It is stored
  under the profile id, or under `credential` when set, so several
  profiles can share one stored key.

Restart the VCA server, then check it with a recording:

```sh
vca transcribe sample.wav
vca transcribe sample.wav --language ko --json
```

### Live dictation

- While someone speaks, VCA sends short chunks for interim text and then a
  final chunk; each is one request. Plan about one request a second per
  person dictating, and a server that answers each chunk well under a
  second (a GPU with `large-v3-turbo`, or `small` on a fast CPU).
- The server runs one transcription at a time, so several people dictating
  at once queue up. Size the hardware and `VCA_STT_MAX_QUEUE` for your
  team; beyond the queue, requests get 503.
- Transcript correction with an LLM is separate (see Step 2 of
  [speech-engines.md](speech-engines.md)) and runs only on final chunks.

### Language

- `[speech] language` sets the primary language (BCP-47, such as `en` or
  `ko-KR`). With only a primary language, VCA sends its base code (`ko`) to
  the server, which skips detection: faster and more reliable for short
  chunks.
- With `secondary_languages` set, VCA does not force a language, so Whisper
  can follow code-switching; it detects the language of each chunk.
- `vca transcribe --language <code>` overrides the setting for one file.

## 9. Privacy

- **Audio stays on your server.** Nothing is sent to a third party: the model
  runs locally after its one-time download from Hugging Face. With a tunnel,
  traffic passes through the tunnel provider encrypted with TLS; choose a
  provider you trust, or a reverse proxy on your own machine.
- **What is logged:** time, user name, client IP, request size, audio
  length, processing time and detected language, plus lockouts and error
  type names. **Never** the audio, the prompt (VCA sends vocabulary hints
  there) or the transcript.
- **What is stored:** uploads are held in memory for the request; uploads
  over 1 MiB may be spooled to a temporary file, which is closed and
  deleted right after reading. Tokens are stored only as hashes.
- **On the VCA side**, VCA never logs keys, dictated text or audio, and keeps
  the token in its credential store on the PC.
- Request logs contain personal data (user names and IP addresses); keep
  the rotation short and limit who can read the data folder.
