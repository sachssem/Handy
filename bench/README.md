# Voice-dictation benchmark suite (fork: voice-control)

A headless, agent-drivable harness for turning "does this model / text-rules
tweak dictate better?" into a measurable number. It runs a corpus of **your own
voice** through an ASR model + pipeline config, scores the output, and writes
comparable JSON + markdown reports.

Your voice never leaves the machine: recordings (`bench/corpus/**/*.wav`) and run
outputs (`bench/results/`) are git-ignored. Only `manifest.toml` is committed.

The harness is the `handy-bench` binary (`src-tauri/src/bin/handy-bench.rs`,
logic in `src-tauri/src/bench/`). Build it with:

```bash
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo build --bin handy-bench
```

(prefix only needed on macOS if you hit the cmake policy error).

## Quick start

```bash
# 1. Record the corpus once (quiet room, ~15 min). Two Enter presses per take.
cargo run --bin handy-bench -- record --corpus bench/corpus

# 2. Run a benchmark across one or more models and compare raw vs text-rules.
cargo run --bin handy-bench -- run \
  --models "turbo=cpp:/path/to/ggml-large-v3-turbo.gguf,parakeet:/path/to/parakeet-tdt-0.6b-v3-int8" \
  --language de --text-rules on --itn on \
  --out bench/results/
```

The report is printed to the terminal and written to
`bench/results/bench-<timestamp>.{json,md}`.

## Real-dictation regression gate

Run from the repository root (the manifest/audio paths are relative to it):

```bash
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo run --manifest-path src-tauri/Cargo.toml \
  --bin handy-bench -- regression

# Override the model; --no-fail retains the report but allows known failures.
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo run --manifest-path src-tauri/Cargo.toml \
  --bin handy-bench -- regression --model 'cpp:/absolute/path/model.gguf' --no-fail
```

The default corpus is `bench/corpus/regression/manifest.toml`: 13 real recordings
imported from local Handy history on 2026-10-04, including five from 2026-10-03.
Each has one `normal.wav` take, supplied spoken/target truth, tags, and provenance.
`--corpus` selects a different corpus. `--settings default` strictly reads the
user's settings and learned dictionary; `--settings none` uses built-in defaults,
and a file path loads an isolated settings snapshot. Missing/unreadable settings
are errors, rather than silently changing the pipeline.

Without `--model`, the selected catalog GGUF is resolved through **hf-hub's own
cache API**, including pinned-revision → `main` fallback. The user's selected
model at import was Qwen3-ASR 1.7B Q8_0. The resolver never downloads models.

Every ready case runs fresh ASR, then the application's
`post_process_transcription_text`: custom words, fillers, normalization,
snippets (with verbatim protection), text rules/ITN, and active learned
corrections, respecting the chosen settings. A windowless Tauri runtime loads
the dictionary into a memory-only store: autosave is disabled and serialization
rejects all disk writes, including legacy migration. No recording, observer,
provider, or app event loop is started. Runtime/model resources are dropped on exit.
`--language auto` (default) uses each case's `de`/`en` tag for pipeline language
evidence; other values pin it. Qwen ignores engine language hints as in `run`.

Self-correction LLM, app styles, and the optional post-process provider need
provider/target-app context and are excluded. `needs_llm = true` cases still run
ASR + the deterministic stages and appear as **LLM-dependent** with their output
and scores; they do not affect the failure count. `status = "pending_truth"`,
an empty target, or `target = "TODO"` skips transcription/scoring. The existing
matrix `run` also skips pending targets.

The compact table and `bench/results/regression-<timestamp>.{json,md}` include
final text, exact match (outer whitespace trimmed), normalized match
(case/punctuation ignored), and spoken WER **on raw ASR**, plus model/settings
metadata in JSON. A normalized match is diagnostic: it cannot hide missing
punctuation or formatting. Only an explicitly flagged
`case_insensitive_path = true` permits **SOFT-PASS** for different path casing;
separators/spelling still must match. Missing recordings/transcription errors
count as failures. Any deterministic failure exits nonzero; `--no-fail` disables
that gate. A run with no deterministic cases scored always fails.

Imported manifest fields extend the existing format:

```toml
[[case]]
id = "real-1791034470-path"
spoken = "Tilde Slash Code Slash Handy"
target = "~/Code/Handy"             # alias of the existing `expected` field
tags = ["de", "path"]
variants = ["normal"]
status = "ready"                   # or "pending_truth"
needs_llm = false
case_insensitive_path = true
[case.provenance]
source_wav = "handy-1791034470.wav"
date = "2026-10-03T15:34:34+02:00"  # history timestamp, with local timezone
history_id = 328
```

History prefixes for email/quotes had already been post-processed at import;
their notes record this, while the supplied spoken truth stays independent of
history/ASR. The truncation case remains `TODO` until the user supplies truth.

## Where the models live (the "empty models dir")

`<app_data>/models/` looks empty even though the app transcribes fine, because
the current HuggingFace GGUF/ONNX catalog is resolved out of the **shared HF hub
cache**, not copied into the app data dir:

- macOS/Linux: `~/.cache/huggingface/hub/models--<org>--<repo>/snapshots/<hash>/...`

`HF_HOME` overrides this according to hf-hub's cache resolver.

Only legacy `Url`/`Local` catalog models land in `<app_data>/models`.

`--models-dir` defaults to that HF hub cache and is used as the base for
**relative** model paths. In practice pass **absolute** paths (or engine-tagged
absolute paths) to the actual `.gguf` file or ONNX model directory.

## Model specs (`--models`)

Comma-separated. Each entry is `[name=][engine:]path`:

| Form                                 | Example                                 | Engine                                     |
| ------------------------------------ | --------------------------------------- | ------------------------------------------ |
| `cpp:` / `whisper:` + `.gguf`/`.bin` | `cpp:/m/turbo.gguf`                     | transcribe-cpp (whisper family, qwen3-asr) |
| `parakeet:` + ONNX dir               | `parakeet:/m/parakeet-tdt-0.6b-v3-int8` | Parakeet ONNX                              |
| `canary:` + ONNX dir                 | `canary:/m/canary-int8`                 | Canary ONNX (uses `--language`)            |
| bare `.gguf`/`.bin` file             | `/m/model.gguf`                         | inferred as transcribe-cpp                 |
| `name=` prefix                       | `turbo=cpp:/m/turbo.gguf`               | sets the display name                      |

A bare directory is ambiguous (Parakeet vs Canary) and must be engine-tagged.
Non-whisper transcribe-cpp archs (e.g. qwen3-asr) reject language hints, so the
harness passes `None` there automatically.

Engine loading mirrors `managers/transcription.rs`; see the "KEEP IN SYNC"
banner in `src-tauri/src/bench/engine.rs`.

## Denoise stage (`--denoise`)

An optional speech-enhancement pass applied to the 16 kHz mono audio **before**
the engine, to test whether denoising helps ASR (it often hurts modern models
via artefacts, so mix-back configs are provided):

| Value        | Effect                                           |
| ------------ | ------------------------------------------------ |
| `none`       | default — raw audio reaches the engine unchanged |
| `dtln`       | full DTLN enhancement                            |
| `dtln-mix70` | `enhanced*0.70 + raw*0.30`                       |
| `dtln-mix50` | `enhanced*0.50 + raw*0.50`                       |

DTLN (<https://github.com/breizhn/DTLN>, MIT) is a two-stage 16 kHz real-time
enhancer (two small stateful ONNX models). The models are downloaded and
sha256-pinned into `<cache>/handy-bench-models/dtln/` on first use (not
committed). The chosen config is recorded in the JSON/MD report metadata.
Implementation: `src-tauri/src/bench/denoise.rs`.

## Corpus format

`bench/corpus/manifest.toml`:

```toml
[[case]]
id = "punct-basic-de"
spoken = "Guten Tag Punkt hallo Komma wie geht es dir Fragezeichen"
expected = "Guten Tag. Hallo, wie geht es dir?"
tags = ["punctuation", "de"]
variants = ["normal", "fast", "noise"]   # audio at punct-basic-de/<variant>.wav
```

- `spoken` — read aloud; the WER/CER reference.
- `expected` — the ideal output after ASR + text rules; the format-accuracy
  reference. Derived from the `text_rules` semantics (see the 28 tests under
  `src-tauri/src/text_rules/`). Newlines in `expected` are real.
- `tags` — group metrics in the report.
- `variants` — recorded audio files (default `normal`, `fast`, `noise`).

The shipped manifest ships ~15 cases covering German prose, DE punctuation
commands, EN keywords, ITN (compounds, decimals, guards), DE/EN code-switching,
paths + casing, short-English "Cyrillic-killers", and filler/self-correction.

## Recording protocol

`handy-bench record` walks the manifest and, for each case/variant without a WAV:

1. prints the sentence,
2. `[Enter]` to start recording (or `s` + Enter to skip),
3. read it aloud,
4. `[Enter]` to stop — saves a 16 kHz mono WAV,
5. `[Enter]` for the next take, or `r` + Enter to immediately re-record the
   same take and overwrite that WAV.

Recording is idempotent by default: already-recorded takes are skipped, missing
takes are recorded, and Ctrl-C is safe for resuming later. At startup the
recorder prints total, already-recorded, and missing counts. If the corpus is
partially recorded, press Enter to continue missing-only, or type `all` + Enter
to re-record every take from scratch. Existing WAVs are overwritten only after a
new take has been captured and saved directly to the final path.

Use `--only <case-id>` to focus on one case. Use repeatable
`--re-record <spec>` for targeted redo, where `<spec>` is either `<case-id>` for
all variants of that case or `<case-id>/<variant>` for one take. With
`--re-record` alone, only matching takes are recorded; missing files elsewhere
are not filled in. With both `--only` and `--re-record`, the effective set is
their intersection. `--overwrite` remains available for re-recording all
selected takes.

One session of ~15 min covers the full corpus. Recording runs in three passes:
all sentences `normal` (natural pace), then `fast` (rushed), then `noise`
(dictation while background audio is playing, e.g. speaker music or café
ambience). The noise take benchmarks robustness against real background audio.

Requires microphone permission for your terminal.

## Adding cases from real failures

Turn real dictation history into cases:

```bash
cargo run --bin handy-bench -- export-history --out bench/corpus --limit 20
```

This reads `<app_data>/history.db` + `<app_data>/recordings/`, copies each
recording into `bench/corpus/hist-<id>/normal.wav`, and writes a reviewable
`bench/corpus/history-stubs.toml` (git-ignored) with `spoken` pre-filled from the
transcription (marked TODO verify) and `expected` empty. Correct each stub, fill
`expected`, change `status` from `pending_truth` to `ready`, and paste the cases
into `manifest.toml`.

## Reading the report

- **Recognition accuracy (vs spoken)** — mean WER / CER per model. This is pure
  ASR quality; independent of the text-rules config. Lower is better.
- **Format accuracy (vs expected)** — the `model × config` matrix. Each cell is
  `mean format accuracy / exact-match rate`. `raw` = text rules off, `rules` =
  text rules on. The lift from `raw` to `rules` is the value of the text-rules
  layer.
- **Per-tag format accuracy** — the same, sliced by tag, so you can see e.g.
  whether `itn` or `punctuation` regressed.
- **Worst 10 cases** — the lowest-scoring case/variant/model under the most
  processed config, with `expected` vs `got` vs `raw`, for eyeballing failures.

Scoring lives in `src-tauri/src/bench/score.rs`: WER/CER are word/char-level
Levenshtein on normalized text (lowercased, punctuation stripped, umlauts kept);
format accuracy is `1 - charEdit/maxLen` against `expected` plus an exact-match
flag.
