# VCA reference TTS server

A small, OpenAI-compatible text-to-speech server for
[Voice Command AI](../../README.md) spoken alerts. It puts the API VCA
expects in front of a TTS engine **that you install yourself**:

- `POST /v1/audio/speech` with `{model, input, voice, response_format, speed, language}`
  answers audio (`wav` and `pcm` always; `mp3`, `opus`, `aac` and `flac` when
  [ffmpeg](https://ffmpeg.org) is on the `PATH`).
- `GET /v1/audio/voices` lists the engine's voices.
- `GET /v1/models` and `GET /health` for clients and checks.

Backends: [MeloTTS](https://github.com/myshell-ai/MeloTTS) and
[Supertonic](https://github.com/supertone-inc/supertonic). This project ships
**no model weights** and downloads none itself; each engine fetches or
documents its own.

Any other OpenAI-compatible TTS server works with VCA too; this one exists so
that a free local setup is a few commands away. See the full guide in
[docs/speech-engines.md](../../docs/speech-engines.md).

## Licences

- This server: MIT, like the rest of VCA.
- MeloTTS: MIT for code and models at the time of writing.
- Supertonic: at the time of writing, sample code under MIT and model weights
  under the OpenRAIL-M licence (with use restrictions).

Licences change and differ between code and weights. **Check each engine's
code and weights licence for your use**, including any requirement to tell
listeners that a voice is machine-generated (VCA can show such a notice with
`disclosure = "..."` on the engine in `config.toml`).

## Install

Python 3.10 or newer. A virtual environment keeps the engine's packages
apart from the rest of your system:

```sh
cd tools/tts-server
python -m venv .venv
# Windows: .venv\Scripts\activate    macOS/Linux: source .venv/bin/activate
pip install -e .
```

Then install **one** engine.

### MeloTTS

MeloTTS is installed from its repository (check its README for the Python
versions it supports; 3.10 or 3.11 is the safe choice):

```sh
git clone https://github.com/myshell-ai/MeloTTS
pip install -e ./MeloTTS
python -m unidic download
```

Start the server (the first start downloads the MeloTTS model for each
language):

```sh
# Languages to load: EN, ES, FR, ZH, JP, KR (default EN)
MELO_LANGUAGES=EN,KR python -m vca_tts_server --backend melo
# Windows PowerShell:  $env:MELO_LANGUAGES="EN,KR"; python -m vca_tts_server --backend melo
```

Voice ids are MeloTTS speakers, such as `EN-US`, `EN-BR`, `EN-AU`, `KR`.
`MELO_DEVICE` picks `cpu`, `cuda` or `mps` (default `auto`).

### Supertonic

Clone the Supertonic repository and get its ONNX models and voice styles as
its README describes (they are large files, usually fetched with Git LFS),
then install the Python requirements of its `py` folder into the same
environment:

```sh
git clone https://github.com/supertone-inc/supertonic
# follow Supertonic's README to download assets/onnx and assets/voice_styles
pip install -r supertonic/py/requirements.txt   # or the packages its README lists
SUPERTONIC_DIR=$PWD/supertonic python -m vca_tts_server --backend supertonic
```

Voice ids are the voice style names (`F1`, `M1`, ...). Optional:
`SUPERTONIC_STEPS` (quality steps, default 5), `SUPERTONIC_GPU=1`,
`SUPERTONIC_ONNX_DIR` and `SUPERTONIC_VOICE_DIR` for other folders.
Multilingual Supertonic releases take a language on every call: the server
uses the `language` VCA sends with each request (`ko`, `en`, ...), else
Korean for text with Hangul, Japanese for text with kana, else
`SUPERTONIC_LANG` (default `en`). The
backend uses the helper module (`py/helper.py`) of the Supertonic repository;
if a newer Supertonic release changes that helper, adjust
`vca_tts_server/backends/supertonic.py`.

## Run

```sh
python -m vca_tts_server --backend melo
```

| Option | Environment | Default |
| --- | --- | --- |
| `--backend` | `VCA_TTS_BACKEND` | `melo` |
| `--host` | `VCA_TTS_HOST` | `127.0.0.1` (this PC only) |
| `--port` | `VCA_TTS_PORT` | `8880` |
| `--max-chars` | `VCA_TTS_MAX_CHARS` | `1000` |
| `--strict-voices` | | off: unknown voices use the default voice |
| | `VCA_TTS_TOKEN` | none: no token needed |

With `VCA_TTS_TOKEN` set, every `/v1` request needs
`Authorization: Bearer <token>`; enter the same token as the engine's
**API key** in VCA. Keep the default `127.0.0.1` unless another machine must
reach the server, and always set a token then.

The server logs only the length of each text, the voice and the time taken,
never the text or the audio.

Quick check:

```sh
curl http://127.0.0.1:8880/v1/audio/voices
curl -X POST http://127.0.0.1:8880/v1/audio/speech -H "Content-Type: application/json" \
  -d '{"input":"Build finished","voice":"EN-US","response_format":"wav"}' -o test.wav
```

## Connect it in VCA

Desktop app on the PC: **Settings > Speech engines on this PC > Spoken
alerts > Add**, keep **Local OpenAI-compatible server**, then:

- Base URL `http://127.0.0.1:8880/v1`, model `tts-1` (any name is accepted),
- Voice: an id from `/v1/audio/voices`,
- Audio format `wav` (or `mp3` with ffmpeg installed),
- **Test**, then **Save**, and restart the VCA server when the app asks.

## Tests

The tests use a fake backend, so no engine or model is needed:

```sh
pip install -e ".[test]"
python -m pytest
```
