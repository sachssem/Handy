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

## Where the models live (the "empty models dir")

`<app_data>/models/` looks empty even though the app transcribes fine, because
the current HuggingFace GGUF/ONNX catalog is resolved out of the **shared HF hub
cache**, not copied into the app data dir:

- macOS: `~/Library/Caches/huggingface/hub/models--<org>--<repo>/snapshots/<hash>/...`
- Linux: `~/.cache/huggingface/hub/...`

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
4. `[Enter]` to stop — saves a 16 kHz mono WAV.

Already-recorded takes are skipped (use `--overwrite` to redo, `--only <case-id>`
to focus). One session of ~15 min covers the full corpus. Recording runs in
three passes: all sentences `normal` (natural pace), then `fast` (rushed), then
`noise` (dictation while background audio is playing, e.g. speaker music or café
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
`expected`, and paste the cases into `manifest.toml`.

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
