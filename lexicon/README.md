# Developer vocabulary (base lexicon)

This directory is **layer 1** of the developer vocabulary engine
(requirements §3.3.1). Speech-to-text engines often mangle technical terms:
`kubectl` becomes "cube control", `useEffect` becomes "use effect", and
`Claude Code` becomes "cloud code". The base lexicon lists the canonical
spelling of each term together with the spoken or misrecognized forms that
should be turned back into it.

The `vca-speech` crate uses these files in two places:

1. **STT hints**: the most relevant terms are sent to the speech engine as a
   vocabulary prompt (OpenAI-compatible APIs, local Whisper) or as phrase hints
   (Google Cloud Speech-to-Text).
2. **Post-recognition correction**: spoken variants found in the transcript
   are replaced with the canonical term. The user sees every replacement
   highlighted and can undo it before sending.

Project terms (layer 2) and personal terms (layer 3) are added at runtime and
take priority over this directory.

## Layout

```
lexicon/
  en/            English: the canonical terms and English spoken forms
    ai.toml          AI agents, models, LLM terms
    cloud.toml       cloud, infrastructure, operations
    concepts.toml    programming concepts and common identifiers
    databases.toml
    frameworks.toml
    git.toml
    languages.toml   programming, markup and query languages
    platforms.toml   operating systems, runtimes, hardware
    tools.toml       package managers, build tools, editors, CLIs
    web.toml         protocols, formats, web platform
  ja/ ko/ zh/ es/ de/ fr/
    common.toml      localized spoken forms of high-frequency terms
```

The directory name is the language of the spoken variants in its files
(ISO 639-1 code). Any `*.toml` file in a language directory is loaded, so you
can add new files (for example `en/gamedev.toml`) without touching code.

## File format

```toml
# Comments are welcome; keep them in English.
language = "en"          # optional, defaults to the directory name
category = "tool"        # optional default category for every entry
tags = ["cli"]           # optional default tags for every entry

[[entry]]
term = "kubectl"                                  # canonical spelling (required)
variants = ["kube control", "cube control"]       # spoken forms in this file's language
category = "tool"                                 # optional, overrides the file default
tags = ["kubernetes"]                             # optional ecosystem or topic tags
weight = 1.5                                      # optional, default 1.0
match_term = false                                # optional, see below
```

| Field | Meaning |
|---|---|
| `term` | Canonical spelling inserted into the transcript. Entries with the same `term` in different files or languages are merged. |
| `variants` | Spoken or misrecognized forms. Matching ignores case, full-width/half-width differences and repeated spaces. Multi-word variants are fine. CJK and Hangul variants match without surrounding spaces, so particles such as `を` or `를` may follow. |
| `category` | Coarse grouping (`language`, `framework`, `tool`, `git`, `cloud`, `ai`, `web`, `database`, `concept`, `platform`). |
| `tags` | Ecosystems or topics such as `rust`, `react`, `kubernetes`. When the active project uses an ecosystem (detected from its manifests), terms with that tag rank higher in hints and win ties between homophones. |
| `weight` | Relative importance. Use 1.2 to 2.0 for very common terms, 0.3 to 0.8 for terms that are useful as hints but rarely misheard. |
| `match_term` | Whether the term's own spelling, typed in a different case, is rewritten to the canonical casing (`github` → `GitHub`). By default this happens only for distinctive spellings (inner capitals, digits or punctuation), so plain words such as `Go` or `React` never rewrite the ordinary words "go" or "react". |

## Contribution guide

Good variants are what a speech engine actually produces for the term. The
best source is your own transcripts: dictate a sentence that uses the term and
copy what came out.

**Do**

- Add multi-word spoken forms: `"use effect"`, `"kube control"`, `"next js"`.
- Add spelled-out acronyms: `"c i c d"`, `"j w t"`.
- Add localized pronunciations in the matching language directory, written the
  way that language's speech engine would output them (katakana for Japanese,
  hangul for Korean, phonetic spelling for Spanish, German and French).
- Use tags so the engine can prefer the right term in context.
- Keep comments and file names in English.

**Do not**

- Add a variant that is an ordinary word or phrase of the language. "view"
  must not map to `Vue`, "this error" must not map to `thiserror`, and
  "read me" must not map to `README` because people say these words in normal
  prompts. When in doubt, leave it out: a missed correction is cheap, a wrong
  one is annoying.
- Add translations. A Chinese or Spanish user who says the translated word
  wants the translated word.
- Map plural forms to singular terms ("pull requests" → "pull request").
- List the same variant under two different terms unless they really are
  homophones; the engine resolves homophones by layer, tags and weight, and
  reports them as conflicts.

**Check your change**

```sh
cargo test -p vca-speech lexicon
```

The tests load every file in this directory, reject syntax errors and unknown
fields, and fail if a variant in the base lexicon maps to two different terms
without a tie-breaker.
