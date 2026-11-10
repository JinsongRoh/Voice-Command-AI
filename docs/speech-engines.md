# Your own speech engines

Voice Command AI (VCA) is open source. It ships **no speech servers, no
cloud accounts and no model weights**. Speech recognition, transcript
correction and spoken alerts all run on engines that **you** install on your
PC or connect with **your own** cloud account. This guide shows how to set
up one free, local engine of each kind, connect it in the app and test it.

| Part | What it does | Needed? | Example in this guide |
| --- | --- | --- | --- |
| Speech-to-text (STT) | Turns your dictation into text on the PC | Optional: without it, each device uses its own speech recognition | faster-whisper-server (Speaches) |
| Correction LLM | Fixes misheard words in finished dictation chunks | Optional | Ollama with a small model |
| Text-to-speech (TTS) | Speaks alerts in a voice of your choice | Optional: without it, devices use their built-in voice | VCA's reference TTS server with MeloTTS or Supertonic |

Everything below runs on the same PC as the VCA server (`vcad`). Local
engines keep your audio and text on that PC. A cloud engine sends them to
the provider you choose, under your account and that provider's terms.

## How VCA connects STT and TTS

All engines are connected to the **VCA server on your PC** (`vcad`), never to
the phone. Phones and other devices only talk to that server over VCA's
end-to-end encrypted link, so engine addresses and keys stay on the PC.

```text
Dictation:   phone mic ──audio──▶ VCA server (PC) ──▶ your STT server ──text──▶ VCA server
                                        │                                            │
                                        └──▶ correction LLM (optional) ◀─────────────┘
                                                                 │
             phone input box ◀──────────── corrected text ───────┘

Spoken alert: agent event ─▶ VCA server ─▶ your TTS server ─audio─▶ VCA server ─▶ phone speaker
                                   └─ no TTS engine or it fails ─▶ the phone's built-in voice
```

- **Protocol:** both use the widely supported OpenAI-compatible HTTP API, so
  any compatible server works:
  - STT: `POST {base_url}/audio/transcriptions` (multipart audio; model
    `whisper-1` or your model name);
  - TTS: `POST {base_url}/audio/speech` with
    `{model, input, voice, response_format}`.
  - `GET {base_url}/models` and, for TTS, `GET {base_url}/audio/voices` list
    what the server offers.
- **Where you connect them:** Settings > **Speech engines on this PC** in the
  desktop app (Step 4), or `config.toml` on the PC.
  - Each engine is a profile: base URL, model, voice/speakers, and a key.
  - The key is stored in the PC's credential store, never in the config
    file and never on a phone.
- **Who chooses what:** each device picks an engine by name. Phones pick the
  STT engine and correction LLM in Settings > Voice > Speech recognition, and
  the alert voice (device voice or a PC engine, and a speaker such as female
  or male) in Settings > Voice > Spoken alerts.
- **Remote servers:** the engine can also run on another machine on your
  network or behind HTTPS (for example a self-hosted Whisper server, see
  [whisper-server.md](whisper-server.md)). Use `https://` and a token whenever
  it is reachable beyond your own PC.
- **Fallbacks:** without an STT engine, a device uses its own speech
  recognition (your keyboard's dictation key also works in the input box).
  Without a TTS engine, alerts use the device's built-in voice.

> **Licences.** Each engine has its own licence for its code and, often, a
> different one for its model weights. The notes in this guide are a
> starting point, not legal advice: **check each engine's code and weights
> licence for your use** (especially commercial use) before you rely on it.

## Before you start

- The VCA desktop app on the PC, with its server running (the app starts it
  by default: Settings > Connect a phone shows "Running").
- For the Docker examples: Docker Desktop (Windows, macOS) or Docker Engine
  (Linux). Every engine can also be installed without Docker; follow its
  own README.
- Pick ports that are free on your PC. This guide uses 8000 (speech-to-text),
  11434 (Ollama) and 8880 (TTS).

## Step 1: speech-to-text with faster-whisper-server

[Speaches](https://github.com/speaches-ai/speaches) (formerly
faster-whisper-server) serves Whisper models through the OpenAI-compatible
`POST /v1/audio/transcriptions`. Its code is MIT-licensed; the Whisper
models it downloads are published by their authors under their own
licences (the Systran faster-whisper conversions of OpenAI Whisper are MIT).

1. Start it on this PC only (CPU image; GPU images are listed in its docs):

   ```sh
   docker run -d --name speaches -p 127.0.0.1:8000:8000 \
     -v speaches-cache:/home/ubuntu/.cache/huggingface/hub \
     ghcr.io/speaches-ai/speaches:latest-cpu
   ```

2. Download a model once (the exact steps can change between Speaches
   versions; see its documentation). `small` is a good start on a CPU;
   `large-v3` is more accurate and needs a GPU:

   ```sh
   curl -X POST http://127.0.0.1:8000/v1/models/Systran/faster-whisper-small
   ```

3. Check that it answers:

   ```sh
   curl http://127.0.0.1:8000/v1/models
   ```

Other OpenAI-compatible speech-to-text servers work the same way (for
example LocalAI or a whisper.cpp server with an OpenAI-compatible endpoint):
only the base URL and model name change.
For a hardened, self-hosted Whisper server see [whisper-server.md](whisper-server.md).

## Step 2: a correction LLM with Ollama

[Ollama](https://ollama.com) (MIT) runs open models locally and offers an
OpenAI-compatible API at `http://127.0.0.1:11434/v1`.

1. Install Ollama from its website, then download a small model:

   ```sh
   ollama pull qwen2.5:1.5b
   ```

   `qwen2.5:1.5b` (Apache-2.0) is fast on a CPU. Larger models such as
   `qwen2.5:7b` (Apache-2.0) correct better when you have the memory. Model
   licences differ between sizes and families, so check the licence of the
   exact model you pull.

2. Check that it answers:

   ```sh
   curl http://127.0.0.1:11434/v1/models
   ```

LM Studio (`http://127.0.0.1:1234/v1`), vLLM (`http://127.0.0.1:8000/v1`) and
LocalAI (`http://127.0.0.1:8080/v1`) work as well: use their base URL and a
model name they serve.

## Step 3: spoken alerts with an OpenAI-compatible TTS server

VCA speaks alerts through any server that offers the OpenAI-compatible
`POST /v1/audio/speech`. Two ways:

**A. VCA's reference TTS server** (in this repository, MIT): a small Python
app in [`tools/tts-server`](../tools/tts-server/README.md) in front of an
engine you install:

- [MeloTTS](https://github.com/myshell-ai/MeloTTS): MIT licence for code and
  models at the time of writing. English (several accents), Spanish, French,
  Chinese, Japanese and Korean.
- [Supertonic](https://github.com/supertone-inc/supertonic): at the time of
  writing the code is MIT and the model weights are under the OpenRAIL-M
  licence, which adds use restrictions. Check which languages the version you
  download supports.

Follow [its README](../tools/tts-server/README.md) to install one engine,
then start it on this PC:

```sh
cd tools/tts-server
python -m vca_tts_server --backend melo      # or --backend supertonic
```

It listens on `http://127.0.0.1:8880/v1` and lists its voices at
`http://127.0.0.1:8880/v1/audio/voices`.

**B. Any other OpenAI-compatible TTS server**, for example openedai-speech
(AGPL-3.0; its voices come from Piper and Coqui XTTS models with their own
licences) or LocalAI. Use its base URL, model and a voice it offers.

Some voice licences require telling listeners that the audio is
machine-generated. VCA can show such a notice: add `disclosure = "..."` to
the engine in `config.toml` (see below).

## Step 4: connect the engines in the app

On the PC, open VCA and go to **Settings > Speech engines on this PC**. (This
section appears only in the desktop app, for the PC's own server. Phones
cannot change engines or see keys.)

For each engine:

1. Click **Add** next to "Speech recognition", "Transcript correction" or
   "Spoken alerts".
2. In **Start from**, keep **Local OpenAI-compatible server** (or choose
   OpenAI, Google Cloud or Anthropic for your own cloud account). A preset
   only fills in the fields; every field stays editable and no preset
   contains a key.
3. Check the **Base URL** and **Model**:

   | Engine | Base URL | Model |
   | --- | --- | --- |
   | Speaches | `http://127.0.0.1:8000/v1` | `Systran/faster-whisper-small` |
   | Ollama | `http://127.0.0.1:11434/v1` | `qwen2.5:1.5b` |
   | Reference TTS server | `http://127.0.0.1:8880/v1` | `tts-1` (any name works), format `wav` |

   For TTS, set **Voice** to one of the ids from `/v1/audio/voices` (for
   example `EN-US` with MeloTTS or `F1` with Supertonic). Under **Voices people
   can pick** you can list several, one per line as `id | label | gender`;
   phones then offer them in their alert settings.
4. **API key**: leave it empty for local servers without a token. For a
   cloud account, or a local server protected by a token, paste the key
   here. It is sent once to the VCA server on this PC, stored in its
   credential store (never in `config.toml`) and never shown again. To
   replace it, type a new one; to keep it, leave the field empty.
5. Click **Test**. VCA sends one tiny real request to your engine and shows
   "Works (123 ms)" or the engine's error:
   - speech-to-text: a built-in one-second tone (no microphone audio),
   - LLM: a one-word prompt,
   - TTS: the short phrase "Voice Command AI test."
6. Tick **Use as the default** if devices without their own choice should
   use it, then **Save**.

Engine changes take effect when the VCA server restarts: the app shows a
**Restart the server** button when it started the server itself (restarting
may stop agent sessions it started; otherwise restart `vcad` where you
started it). Keys apply right away.

On each phone or other device, pick the engines in **Settings > Voice >
Speech recognition** (engine and correction LLM) and **Settings > Voice >
Spoken alerts** (spoken alerts from the PC). Devices see the engines by name
only.

## Cloud accounts

Use the presets **OpenAI**, **Google Cloud** (speech-to-text) or **Anthropic**
(correction) with your own key from that provider. Usage is billed to your
account and your audio or text is processed by that provider under its
terms. Google Cloud also needs your project id, and either an API key or an
access token in an environment variable (`access_token_env` in `config.toml`).

## Without the app: config.toml

Everything the app edits lives in the server's `config.toml`
(`%APPDATA%\vca\vca\config\config.toml` on Windows,
`~/Library/Application Support/dev.vca.vca/config.toml` on macOS,
`~/.config/vca/config.toml` on Linux). The app keeps your comments and every
other setting, and saves the previous file as `config.toml.bak`.

```toml
[speech]
default_engine = "local-stt"
default_llm = "local-llm"

[[speech.engines]]
id = "local-stt"
label = "Speech server (this PC)"
base_url = "http://127.0.0.1:8000/v1"
model = "Systran/faster-whisper-small"

[[speech.llms]]
id = "local-llm"
label = "Local LLM (this PC)"
base_url = "http://127.0.0.1:11434/v1"
model = "qwen2.5:1.5b"

[tts]
default_engine = "local-tts"

[[tts.engines]]
name = "local-tts"
base_url = "http://127.0.0.1:8880/v1"
model = "tts-1"
voice = "EN-US"
format = "wav"
# disclosure = "This voice is machine-generated."
```

Keys never go in this file. Store them with `vca speech login <id>` or
`vca tts login <name>` on the PC, or name an environment variable with
`api_key_env = "MY_KEY_VARIABLE"`.

## Engines behind an authenticating gateway

Some engines sit behind a gateway or reverse proxy that authenticates every
request with its own bearer token (for example a zero-trust tunnel). The
gateway checks `Authorization: Bearer <gateway token>`, removes that header
and forwards the request, so the engine's own API key has to travel in
another header. Two optional settings per engine handle this. They apply to
OpenAI-compatible speech-to-text engines, correction LLMs and TTS engines
(not to the Google Cloud or Anthropic APIs):

- **Key header** (`api_key_header`): the header that carries the API key,
  sent as is (no `Bearer` prefix), for example `X-VCA-Key`. Without it, the
  key goes in `Authorization: Bearer <key>` as before. Ask the engine's
  operator which header the server accepts.
- **Gateway token**: sent as `Authorization: Bearer <token>` on every
  request to that engine (transcription, correction, the built-in
  conductor, spoken alerts and the app's **Test** button). It is a secret
  and is stored like the API key, in the credential store on the PC, never
  in `config.toml`. Alternatively name an environment variable with
  `gateway_token_env`.

A gateway token needs a key header other than `Authorization` (both would
otherwise use the same header): VCA refuses that combination with an error
that says so.

In the app, both are under **Advanced** when you add or edit an engine. On
the command line (the profile id or engine name must already exist):

```sh
# Speech-to-text engines and correction LLMs
vca speech key-header <id> X-VCA-Key   # --clear goes back to Authorization: Bearer
vca speech gateway-login <id>          # prompts for the token (hidden), or reads stdin
vca speech gateway-logout <id>

# TTS engines
vca tts key-header <name> X-VCA-Key
vca tts gateway-login <name>
vca tts gateway-logout <name>
```

In `config.toml`:

```toml
[[speech.engines]]
id = "team-stt"
base_url = "https://engine.example.com/v1"
api_key_header = "X-VCA-Key"
# gateway_token_env = "TEAM_GATEWAY_TOKEN"   # instead of gateway-login
```

A changed key header (like any engine setting in `config.toml`) is used
after the VCA server restarts. Stored keys and gateway tokens are read on
every request, so replacing or deleting one applies at once, without a
restart.

## Troubleshooting

| Test result | What to check |
| --- | --- |
| `http error: ... connection refused` | The engine is not running, or listens on another port. Open the base URL's `/models` (or `/audio/voices`) in a browser on the PC. |
| `... returned 404` | The base URL usually ends in `/v1`. For speech-to-text, the model may not be downloaded yet. |
| `... returned 401` or "has no key yet" | Enter the key (or the server's token) and save. Behind a gateway, check the gateway token and the key header. |
| `... returned 400` for TTS | Use a voice and format the server supports (the reference server needs ffmpeg for mp3). |
| Works, but devices do not offer it | Restart the VCA server so it loads the new engines. |

## Security notes

- Keep local engines bound to `127.0.0.1` (the commands above do). If an
  engine must listen on your network, protect it with a token and enter the
  token as its key in VCA.
- Only the desktop app on the PC can read or change engine settings and
  keys; paired phones and remote connections are refused.
- VCA never logs keys, dictated text, alert text or audio.
