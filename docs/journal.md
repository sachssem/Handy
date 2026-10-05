# Dictation journal — guide for analysis agents

The fork writes a local journal of every dictation so quality can be studied
and improved without asking the user. This page is the contract: what is
recorded, how to join it, and ready-made queries. Implementation:
`src-tauri/src/journal/` (schema in `record.rs`), feature notes in
[`fork-patches.md`](fork-patches.md#dictation-journal--app-context).

## Where

- `~/Library/Logs/com.pais.handy/journal/YYYY-MM-DD.jsonl` (macOS; generally
  `<app log dir>/journal/`, portable mode `Data/logs/journal/`). The Tauri
  command `get_journal_dir_path` returns the exact path; Settings → Advanced →
  History shows it.
- One file per **local** day, by the time a line was written. Kept
  `dictation_journal_retention_days` (default 90; Settings offers 30 / 90 /
  180 / 365) days.
- Switch: `dictation_journal_enabled` (default on). While off, nothing is
  recorded (not even in memory). Nothing leaves the machine.

## Lines

Every line is one JSON object with the envelope

| field  | meaning                                                         |
| ------ | --------------------------------------------------------------- |
| `v`    | schema version (currently `1`)                                  |
| `ts`   | epoch ms when the line was written                              |
| `type` | `dictation` \| `context` \| `learning` \| `overlay` \| `cap`    |
| `id`   | dictation id = epoch ms of the shortcut press (strictly rising) |

`id` joins all event types of one dictation. A `learning` line may have
`id: null` (session without a preceding journaled dictation). `cap` means the
day hit the 32 MB safety cap; later lines of that day were dropped.

Lines of one dictation are not adjacent and not ordered: `context` usually
lands first (≈ 50–150 ms after the press), `dictation` when the record ends
(after paste), `learning` when the post-paste session ends (seconds to
minutes later, possibly in the next day's file), `overlay` whenever the
webview reports.

### `dictation`

| field                                       | meaning                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| ------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `binding`, `post_process`                   | shortcut binding (`transcribe` / `transcribe_with_post_process`)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| `start_ms`, `stop_ms`, `end_ms`             | press, stop request, record end (epoch ms)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `stop_trigger`, `recording_limit_auto_stop` | stop shortcut string; `true` when the recording-limit auto-stop ended it                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| `start_path`                                | press-path spans in ms: `model_kickoff_ms`, `stream_plan_ms`, `overlay_ms`, `tray_ms`; plus `selected_model`, `streaming`, `vad`                                                                                                                                                                                                                                                                                                                                                                                                                  |
| `mic_ready_ms`                              | press → first microphone samples                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| `recorded_wall_secs`, `retained_audio_secs` | press → stop vs. audio kept after VAD edge trimming                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| `wav_file`, `wav_saved`                     | recording in the app's `recordings/` dir                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| `asr_bias`                                  | one entry per transcribe-cpp run in run order (stream start, primary, unbiased retry after a rejected bias, pinned retry, fallback model): `bias_vocab_n`, `bias_prompt_chars` — counts only, never the prompt text. Omitted when no run reached the bias stage (e.g. ONNX engines).                                                                                                                                                                                                                                                              |
| `model_load_ms`, `model_warmup_ms`          | only when the press had to load the model (not resident, e.g. after `model_unload_timeout`): load call ms including the warm-up, and the warm-up inference ms within it (transcribe-cpp only; absent when skipped or `HANDY_MODEL_WARMUP=0`)                                                                                                                                                                                                                                                                                                      |
| `asr`                                       | batch engine run: `model_id`, `engine`, `backend` (bound compute backend, `onnx` for ONNX), `accelerator_setting`, `language_setting` (intent), `language_effective` (after model coercion), `language_hint` (passed to the run; null = auto), `detected_language` (model LID), `language_evidence`, `translated`, `engine_ms` (incl. model-load wait and guard retries), `audio_secs`. Absent for live-streaming runs.                                                                                                                           |
| `allowlist_guard`                           | only when the guard fired: `reason`, `allowlist`, `action` (`fallback_model` \| `pin_retry`), `fallback_model`, `primary_text`, `result` (`fallback_accepted` \| `fallback_rejected` \| `pin_retry_ok` \| `pin_retry_failed` \| `pin_unsupported`), `result_text`                                                                                                                                                                                                                                                                                 |
| `transcription_ms`                          | stop pipeline: stream finalize / batch transcribe call                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| `text`                                      | text after each stage, in order: `asr` → `custom_words` (null = not applied, e.g. passed as whisper prompt) → `fillers` → `normalized` → `snippets` (null = no snippet fired) → `text_rules` → `learned` → `llm` (null = no LLM result) → `self_correction` (null = not applied) → `app_style` (null = stage off) → `final` (what was pasted)                                                                                                                                                                                                     |
| `text_redacted`                             | Why all text was dropped (fails closed): `secure_field` password field, `secure_input` secure event input on, `unknown_field` no captured context or no focused element identified, `unverified_platform` non-macOS (no Accessibility API, text never journaled)                                                                                                                                                                                                                                                                                  |
| `llm`                                       | `requested`, `applied`, `ms` (output handling incl. the LLM call)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| `self_correction`                           | only when a self-correction cue was found: `requested` (pass enabled), `applied`, `ms`, `reason` (`applied` \| `disabled` \| `snippet` \| `post_process_applied` \| `no_provider` \| `apple_unavailable` \| `timeout` \| `error` \| `empty` \| `unchanged` \| `length_ratio` \| `added_words` \| `unrelated_deletion`), `cue` (e.g. `cue:ne`), `provider` (`rules` for deterministic slot repairs, otherwise the LLM provider id), optional `candidate` (cleaned LLM output on rejection, including unchanged; omitted whenever text is redacted) |
| `paste`                                     | `method`, `ms`, `ok`, `error`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| `outcome`                                   | `pasted` \| `paste_failed` \| `not_pasted` \| `empty` \| `cancelled` \| `error` \| `start_failed` \| `abandoned` (never stopped — cancelled while recording; written when the next dictation starts)                                                                                                                                                                                                                                                                                                                                              |
| `error`                                     | transcription / start error message                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |

Rejected self-correction candidates are recorded only in this local journal,
never in info/debug logs. `self_correction.candidate` is absent for accepted
results, skipped calls and deterministic repairs. All existing text-redaction
reasons also remove the candidate while retaining non-text pass facts.

### `context`

Captured at recording start, off-thread: `id` (the dictation id),
`captured_at_ms` (epoch ms the capture started), `bundle_id`, `app_name`,
`window_title`, `focused_role`, `focused_subrole`, `secure` (password field:
its text is never read, its window title dropped), `secure_input` (secure
event input was on: no focused element was read at all), `text_before_caret`
(≤ 500 chars), `selection_len` (UTF-16 units), `caret_at_start` (only
present, as `true`, when the caret was read at offset 0 — field start or
empty field), `text_source`
(`string_for_range` \| `value`), `capture_ms`, `error` (`secure_input`,
`no_frontmost_app`, `no_focused_element`, `budget_exhausted`,
`value_too_large`, …), `text_redacted`.

The line follows the dictation's redaction rule: when `text_redacted` is set
(same reasons as on `dictation`), `window_title`, `text_before_caret` and
`text_source` are null.

### `learning`

The post-paste correction-learning session: `session` (matches the
`learn: session N` debug log lines), `app`, `lang`, `window_secs`,
`observer`, `skipped` (why no session ran), `anchored` (`exact` \|
`paste_only`), `reads`, `edits_observed`, `unrelated`, `gate_rejections`
(`{gate: count}`), `reformulations`, `oversized`, `pending_sets`,
`pending_dropped`, `end_reason`, `committed` (`[{misheard, intended, outcome,
pair_id}]`, outcome as in the store: `suggested`, `promoted`, `re-observed`,
`reverted+blocked`, `blocked`, `manual kept`, `inverse of manual`,
`store full`), `duration_ms`.

### `overlay`

`state` (`recording`, `transcribing`, …), `phase` (`handler` \|
`first_frame`), `epoch_ms`, `since_press_ms`. Reported by the overlay webview
(`RecordingOverlay.tsx`) through the `journal_overlay_stage` command, which
also writes the same breadcrumb to the app log at debug level
(`toast-webview: overlay: show '<state>' handler|first-frame epoch_ms=…`).

## Queries

```bash
J=~/Library/Logs/com.pais.handy/journal
cat $J/*.jsonl > /tmp/j.jsonl   # or a date range: cat $J/2026-10-0*.jsonl

# Per-model latency: count, mean engine ms, mean real-time factor
jq -s '[.[] | select(.type=="dictation" and .asr)] | group_by(.asr.model_id)
  | map({model: .[0].asr.model_id, n: length,
         engine_ms: (map(.asr.engine_ms) | add / length | floor),
         rtf: (map(.asr.engine_ms / 1000 / (.asr.audio_secs + 0.001)) | add / length)})' /tmp/j.jsonl

# Press → mic ready / overlay first frame (p50-ish via sort)
jq -s '[.[] | select(.type=="dictation" and .mic_ready_ms) | .mic_ready_ms] | sort | .[length/2|floor]' /tmp/j.jsonl
jq -c 'select(.type=="overlay" and .state=="recording" and .phase=="first_frame") | .since_press_ms' /tmp/j.jsonl

# What the text rules changed
jq -c 'select(.type=="dictation" and .text.text_rules != .text.normalized)
  | {id, before: .text.normalized, after: .text.text_rules}' /tmp/j.jsonl

# Learned corrections that fired
jq -c 'select(.type=="dictation" and .text.learned != .text.text_rules)
  | {id, before: .text.text_rules, after: .text.learned}' /tmp/j.jsonl

# Allowlist guard: fire rate and results
jq -s '[.[] | select(.type=="dictation" and .asr)] as $d
  | {dictations: ($d|length), fired: ([$d[] | select(.allowlist_guard)] | length),
     results: ([$d[] | .allowlist_guard.result // empty] | group_by(.) | map({(.[0]): length}) | add)}' /tmp/j.jsonl

# Learning gates: which gate rejects most, and outcomes
jq -s '[.[] | select(.type=="learning") | .gate_rejections | to_entries[]]
  | group_by(.key) | map({gate: .[0].key, n: (map(.value) | add)}) | sort_by(-.n)' /tmp/j.jsonl
jq -s '[.[] | select(.type=="learning") | .committed[].outcome] | group_by(.) | map({(.[0]): length}) | add' /tmp/j.jsonl

# Join: dictations with their app context (by id)
jq -s '(map(select(.type=="context")) | INDEX(.id)) as $ctx
  | [.[] | select(.type=="dictation") | {id, app: $ctx[.id|tostring].bundle_id, final: .text.final}]' /tmp/j.jsonl

# Self-correction: cue hits and why the LLM result was (not) applied
jq -s '[.[] | select(.type=="dictation" and .self_correction) | .self_correction.reason]
  | group_by(.) | map({(.[0]): length}) | add' /tmp/j.jsonl

# Outcomes overall
jq -s '[.[] | select(.type=="dictation") | .outcome] | group_by(.) | map({(.[0]): length}) | add' /tmp/j.jsonl
```

## Schema changes

Bump `SCHEMA_VERSION` in `src-tauri/src/journal/mod.rs` on any incompatible
change (rename, removal, meaning change) and log it here; adding a field is
compatible and needs no bump.

- `1` — initial schema. Later compatible additions: `text.snippets`,
  `text.self_correction`, `text.app_style`, `self_correction`, `asr_bias`,
  `context.secure_input`, `context.text_redacted`.
