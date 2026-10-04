# Fork Patch Index (voice-control)

Source of truth for **what the fork adds on top of upstream and how to check each
piece is still needed** after an upstream update. It is the fork's memory: without
it, an upstream sync is a guessing game about which hooks matter and whether
upstream has since shipped an equivalent.

- **When you sync upstream** (`scripts/sync-upstream.sh`): if a rebase conflict
  touches one of these features, run that feature's **Upstream check** _before_
  resolving. If upstream now does the same thing, **drop the feature** instead of
  adapting it (remove its files, its probe in `scripts/fork-check.sh`, and its
  section here — in one commit).
- **When you add a fork feature:** add a section here _and_ a probe in
  `scripts/fork-check.sh`. A feature with no probe can vanish on the next rebase
  unnoticed.
- **Integrity is machine-checked:** `scripts/fork-check.sh` asserts every hook
  below still exists. Run it after any edit to an upstream-owned file.

Conventions used throughout:

- **New files** = fork-owned modules; upstream never touches them, so they never
  conflict. Free to change.
- **Upstream files touched** = graft points; every fork edit inside them carries a
  `fork(voice-control):` marker so the graft is greppable against `main`.
- **Probe** = the grep in `scripts/fork-check.sh` that guards the hook.

---

## Feature summary

| Feature                                                                                  | Kind    | Risk on rebase | Upstream files touched                                                                                                                                |
| ---------------------------------------------------------------------------------------- | ------- | -------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| [text-rules engine](#text-rules-engine)                                                  | feature | low            | `managers/transcription.rs`, `settings.rs`, `lib.rs`, settings UI                                                                                     |
| [correction learning](#correction-learning)                                              | feature | medium         | `actions.rs`, `managers/transcription.rs`, `settings.rs`, `lib.rs`, `cli.rs`, settings UI                                                             |
| [language-allowlist guard](#language-allowlist-guard)                                    | feature | medium         | `managers/transcription.rs`, `shortcut/mod.rs`, `settings.rs`, `ModelSettingsCard.tsx`                                                                |
| [recording-limit auto-stop](#recording-limit-auto-stop)                                  | feature | low            | `settings.rs`, `shortcut/mod.rs`, `transcription_coordinator.rs`, `managers/model.rs`, settings UI                                                    |
| [dictation journal + context](#dictation-journal--app-context)                           | feature | medium         | `actions.rs`, `managers/transcription.rs`, `transcription_coordinator.rs`, `overlay.rs`, `RecordingOverlay.tsx`, `settings.rs`, `lib.rs`, settings UI |
| [ASR biasing](#asr-biasing-vocabulary--app-context)                                      | feature | medium         | `managers/transcription.rs`, `settings.rs`, `lib.rs`, `ModelSettingsCard.tsx`                                                                         |
| [overlay: compact capsule + latency](#overlay-compact-capsule--latency)                  | feature | medium         | `RecordingOverlay.tsx`, `RecordingOverlay.css`, `overlay.rs`, `actions.rs`                                                                            |
| [vad-edges: dictation keeps internal pauses](#vad-edges-dictation-keeps-internal-pauses) | fix     | medium         | `audio_toolkit/audio/recorder.rs` (+ its `tests.rs`)                                                                                                  |
| [benchmark harness](#benchmark-harness)                                                  | tooling | very low       | `lib.rs` (one `mod`), `Cargo.toml`                                                                                                                    |
| [fork build & maintenance tooling](#fork-build--maintenance-tooling)                     | tooling | none           | none (fork-owned scripts)                                                                                                                             |

"Risk on rebase" = likelihood upstream edits the same lines. New-module features are
low; features that graft into `transcription.rs` are the ones to watch.

---

## text-rules engine

Deterministic punctuation / inverse-text-normalization layer applied to ASR output
before paste (spoken "comma" → ",", spacing policy, custom substitutions, ITN).

Pass order (`text_rules::apply_rules`): ITN → lists → quotes → links → context-gated
substitutions → lone-token period strip. Behaviour (precision over recall — a
missed command is cheaper than corrupted prose; details in each module's docs):

- **Context gating** (`context.rs`): built-in command words fire only in command
  context; user rules stay unconditional. A determiner/possessive/contracted
  preposition directly before the word (`der`, `ein`, `zum`, `um`, `the`, `a`, `to`
  …), also across attributive adjectives (`der springende Punkt`), keeps it a
  word (including English adjectives and German prepositions such as `mit`).
  Glue and punctuation commands additionally need a text edge / ASR punctuation
  on either side or an adjacent glue command / path / identifier context;
  punctuation commands also need a word before them. Existing comma-after-greeting
  and bounded two-word hyphen/underscore identifier fragments supply a clause
  signal even without ASR punctuation; `Punkt`/`dot` additionally need a clause-end signal (punctuation,
  line break, end of text, a line-break command, or its own ASR segment before a
  capitalized word) — an unwrapped mid-sentence `Punkt` stays a word.
- **Links** (`links.rs`): `w Punkt|dot w …` joins to `.` (ASR commas/periods around
  the spoken dot dropped) when it ends in a known TLD / file extension, starts with
  `www`, or is all digits (versions); domains lowercased, file stems keep case.
  `local at|ät domain.tld` → e-mail (key `at`) only with a positive signal:
  explicit `ät` / `Klammeraffe`, a spoken-chain local part, or a recipient cue
  within three preceding words. Ordinary “Sign up at example.com” stays prose.
  A lone path/URL/e-mail result
  loses the ASR's trailing period.
- **Quotes** (`quotes.rs`, keys `Anführungszeichen` / `quote`): explicit
  `auf/zu`, `open/close/end quote`, `unquote`; bare English `quote` requires an
  explicit closer; bare German `Anführungszeichen` cannot open before a determiner.
  Ambiguous pairs enclose at most eight words. `in Anführungszeichen X` scare
  quotes stop before the next quote marker, preserving later pairs. Straight
  `"` (safe in code/terminals/chat). Adjacent ASR straight/curly marks around
  resolved spoken openers/closers are absorbed, producing one pair. Sentence
  stops on an ASR closing mark keep their inside/outside position; a duplicate
  stop after the spoken closer is dropped. Generated quoted sentences ending
  in `.`, `!` or `?` inside the quote capitalize their first letter; fragments
  retain their casing. Balanced ASR quotes without spoken commands stay untouched.
- **Lists** (`lists.rs`, keys `Punkt eins`, `number one`, `erstens`, `first`,
  `nächster Punkt`, `next item`): ≥ 2 markers, numbered/ordinal ones in order from
  1; ordinals only at segment start with ≤ 3-word items and a label / colon
  before the first ordinal or at least three items. Verb / pronoun / particle-led
  ordinal items and finite-verb-led numbered items stay prose. Label line + one
  `1. item` / `- item` per line.
- New built-ins: `Tilde` → `~`, `Klammeraffe` → `@`. Structural keys appear in the
  built-ins list (UI) so each pass can be disabled individually.

- **New files:** `src-tauri/src/text_rules/` (`mod.rs`, `itn.rs`, `substitutions.rs`,
  `context.rs`, `links.rs`, `lists.rs`, `quotes.rs`),
  `src/components/settings/TextRules.tsx`, `docs/llm-cleanup-prompt.md`.
- **Upstream files touched:** `managers/transcription.rs` (one call:
  `text_rules::apply_text_rules` in the output stage), `settings.rs`
  (`text_rules_*` fields), `lib.rs` (`mod text_rules;` + tauri command exports),
  `components/settings/advanced/AdvancedSettings.tsx` (mounts `TextRules`).
- **Probe:** `text-rules: *` in `scripts/fork-check.sh`.
- **Upstream check:** does upstream now post-process transcription text
  (punctuation/ITN)? Look in `managers/transcription.rs` around the paste/output
  stage and any new `text`/`format` module:
  ```bash
  git grep -iE "punctuat|inverse.?text|\bitn\b|normaliz" upstream/main -- src-tauri/src
  ```
  If upstream added an equivalent normalization stage, evaluate replacing ours;
  otherwise keep. Keep regardless if ours is materially better (custom rules UI).

## correction learning

Wispr-Flow-style vocabulary learning: after a paste, reads the focused field via
macOS Accessibility, anchors the pasted span in it and diffs the user's edit
inside that span. Gated pairs (names, jargon — not grammar: inflections,
numbers, everyday-word swaps are rejected) are stored as **suggestions**; a
pair becomes **active** (applied on future transcriptions) when observed again
or confirmed. Undo (toast or UI) removes an auto pair and blocks it for good;
manually reverting an applied pair is detected as its inverse (before any
vocabulary gate) and blocks it. The master switch ("Personal dictionary",
`learn_corrections_enabled`) applies the dictionary; the macOS-only sub-switch
"Learn from my corrections" (`learn_from_edits_enabled`, default on) controls
the post-paste watcher alone. Disabling either switch cancels the active watcher
and drops uncommitted candidates, including a quick disable/re-enable. An
anchored composer clearing on submit ends the session even after its previous
correction already settled. Learning journal pair texts follow the originating
dictation's saved redaction decision; counts, pair ids and outcomes survive.

- **Gates:** `empty`, `phrase_length`, `case_or_punctuation`, `number`,
  `distance`, `likely_typo`, `inflection`, `common_words`, `phonetic`.
  `likely_typo` rejects small edits (≤2 edits or relative distance ≤0.34)
  from a common de/en word to an unknown word, unless the target is
  distinctive (inner capitals, an all-caps acronym, or mixed digits/letters).
  First names are known and distinctive case-insensitively, even when also
  common words (`mark → marc`, `Mark → Marc`); case-only edits remain rejected.
  Field diffs split `@`, `.`, `/` into separate tokens, so e-mail local-part
  and URL corrections yield individual word pairs without losing byte offsets.
- **Toast focus:** the toast panel never becomes key
  (`can_become_key_window: false`, `focusable(false)`) and is revealed via the
  nspanel API (`orderFrontRegardless`), never `WebviewWindow::show` (tao's
  `makeKeyAndOrderFront:`) — otherwise it captured the keyboard and e.g. a
  Return meant for a chat composer was lost. Clicks still work through
  `accept_first_mouse(true)` + the non-activating style mask.
- **Toast shortcuts:** Accept (`learned_toast_accept_shortcut`, default
  `ctrl+enter`, plus the keypad-Enter twin) and Never / Undo
  (`learned_toast_dismiss_shortcut`, default `ctrl+backspace`) are registered
  only while a toast is visible (`toast_shortcuts.rs`: armed on reveal,
  disarmed on every hide, generation-guarded take-once claim), through the
  active keyboard backend. Each reveal carries its own event into the main-thread
  closure; delayed hides match the current toast id/generation, and
  `hide_learned_toast(id: Option<String>)` also supports legacy unscoped calls.
  Reconciliation uses blocking workers. Shortcut capture suspends transient
  registrations and claims, restoring only the current visible toast afterwards.
  Bare/Shift-only combos return `needs-modifier`; conflicts retain the
  `conflict:<binding_id>` marker. Editable under "Learning options"
  (`LearnedToastShortcutInput.tsx`); the toast buttons show them as key hints.
- **New files:** `src-tauri/src/correction_learning/` (`mod.rs` apply stage +
  language, `ax_reader.rs`, `commands.rs` Tauri commands, `differ.rs` gates,
  `lexicon.rs` + `data/` word lists, `session.rs`, `store.rs` lifecycle +
  persistence, `toast.rs`, `toast_shortcuts.rs`),
  `src/components/settings/LearnedCorrections.tsx`,
  `src/components/settings/LearnedToastShortcutInput.tsx`, `src/toast/*`, `src/components/icons/RemoveIcon.tsx`,
  `docs/design/auto-learn-corrections.md`.
- **Data licences:** `data/common_de.txt` / `data/common_en.txt` are adapted
  from hermitdave/FrequencyWords (OpenSubtitles 2018, **CC BY-SA 4.0**,
  attribution in the file headers); `data/names.txt` is BSD `propernames`.
- **Storage:** the pairs and the block list live under their own key
  (`learned_corrections`) in `settings_store.json`, not in `AppSettings`, so
  learning never races whole-settings writes. The legacy
  `AppSettings.learned_corrections` list is migrated once (marked by
  `learned_corrections_migrated`, so a stale settings write cannot resurrect
  deleted pairs) and emptied; the retired `learn_corrections_log_only` key is
  ignored.
- **Upstream files touched:**
  - `lib.rs`: `mod correction_learning;`, `correction_learning::init` + toast
    init at startup, `correction_learning::commands::*` registrations in
    `collect_commands!` (incl. `toast_shortcuts::change_learned_toast_shortcut_setting`),
    specta events, `--debug-toast` forwarding.
  - `shortcut/handler.rs`: routes the transient toast bindings to
    `toast_shortcuts::handle_shortcut_event` before the `ACTION_MAP` lookup.
  - `shortcut/mod.rs`: marked suspend/resume hooks include transient toast
    shortcuts in settings shortcut capture.
  - `bindings.ts`: optional toast id on the generated hide command.
  - `cli.rs`: the hidden `--debug-toast` flag.
  - `settings.rs`: `learn_corrections_*`, `learn_from_edits_enabled` and
    `learned_toast_*_shortcut` fields, legacy `learned_corrections`.
  - `actions.rs`: `begin_session` after paste.
  - `managers/transcription.rs`: `apply_learned` with the transcription's
    language evidence.
  - `stores/settingsStore.ts`, `AdvancedSettings.tsx`; the collapsible rows
    use the shared fork-owned `components/ui/Disclosure.tsx` (also used by
    text rules, smart formatting and the language allowlist).
- **Probe:** `correction-learning: *` in `scripts/fork-check.sh` (the commands
  probe targets `correction_learning::commands::` in `lib.rs`).
- **Upstream check:** does upstream ship any "learn from edits" / dictionary /
  custom-replacement feature?
  ```bash
  git grep -iE "learn|correction|replacement|dictionary|vocab" upstream/main -- src-tauri/src src
  ```
  Unlikely (upstream is frozen). If a rebase conflicts here, adapt carefully
  rather than dropping; verify with `handy --debug-toast` (see AGENTS.md "Fork
  Debug Helpers").

## language-allowlist guard

Constrains "auto" language detection to an allowlist; when the detected language
falls outside it, retries pinned to the first allowlisted language and can escalate
to a fallback model. The script predicate also rejects any whitespace-separated
token with at least three Unicode alphabetic letters of an unsupported script,
even inside Latin text (e.g. `Schreib an mark этеxampelpunkt com.`). Digits,
punctuation, emoji and symbols do not count. Cyrillic names of three or more
letters inside German intentionally trigger; `Winkel α beträgt` stays in bounds.
The existing dominant-script threshold remains >60% over at least four letters.
Fallback output must pass this same predicate even if its reported language is
allowlisted.

- **New files:** `src/components/settings/LanguageAllowlist.tsx` (allowlist +
  fallback model, inside the shared `components/ui/Disclosure.tsx`).
- **Upstream files touched:** `managers/transcription.rs` (allowlist guard +
  `run_allowlist_fallback` around the detection stage;
  `script_outside_allowlist` / `fallback_output_in_bounds` token checks), `shortcut/mod.rs`
  (`change_language_allowlist_setting`, fallback-model setter), `settings.rs`
  (`language_allowlist`, `language_allowlist_fallback_model`), `lib.rs`
  (command registrations), `general/ModelSettingsCard.tsx` (mounts
  `LanguageAllowlist`), `stores/settingsStore.ts`, `bindings.ts`.
- **Probe:** `language-allowlist: *` in `scripts/fork-check.sh`.
- **Upstream check:** did upstream add language pinning / detection constraints,
  including mixed-script token detection and fallback validation?
  ```bash
  git grep -iE "allowlist|language_pin|detect.*language|restrict.*language" upstream/main -- src-tauri/src
  ```
  Watch `transcription.rs` detection-stage churn — this is the second-largest graft
  after text-rules and shares the file with it.

## recording-limit auto-stop

Auto-stops a recording just before a model's safe dictation limit (some models
degrade or hang past a length), for models with a measured limit.

- **New files:** `src/components/settings/RecordingLimitAutoStop.tsx`; measured
  limits captured via the bench harness (`probe-limit`).
- **Upstream files touched:** `settings.rs` (`auto_stop_recording_on_limit`),
  `shortcut/mod.rs` (settings command), `transcription_coordinator.rs`
  (`schedule_recording_limit` — the actual auto-stop enforcement, scheduled per
  recording session; stops with `journal::AUTO_STOP_TRIGGER`),
  `managers/model.rs` (per-model limit metadata), `AdvancedSettings.tsx`,
  `RecordingOverlay.tsx` (countdown ring).
- **Probe:** `recording-limit: *` in `scripts/fork-check.sh` (incl. the
  `schedule_recording_limit` enforcement hook in `transcription_coordinator.rs`).
- **Upstream check:** did upstream add a max-recording-length / auto-stop?
  ```bash
  git grep -iE "max.?record|recording.?limit|auto.?stop|duration.?limit" upstream/main -- src-tauri/src
  ```

## dictation journal + app context

Local, structured record of every dictation so an analysis agent can study and
improve quality later without asking the user. One JSON object per line, one
file per local day under `<app log dir>/journal/YYYY-MM-DD.jsonl` (macOS:
`~/Library/Logs/com.pais.handy/journal/`). Schema (versioned, `v` on every
line), join rules and example queries: [`docs/journal.md`](journal.md).

- **Events:** `dictation` (timings, model/engine/backend, language hint +
  detection + evidence, allowlist guard decision/fallback/result, the text after
  every pipeline stage, LLM, paste method/ms, WAV name, auto-stop, outcome),
  `context` (app context captured at recording start), `learning` (post-paste
  session outcome: reads, edits, gate rejections by gate, committed pairs),
  `overlay` (webview show latencies reported via the journal's
  `journal_overlay_stage` command). All keyed by the dictation id (epoch ms of
  the press).
- **Never on the hot path:** hooks only update an in-memory record; finished
  records go over a bounded channel to one writer thread (`try_send`, a full
  channel drops the line). Disk errors are logged rate-limited and never reach
  the pipeline. Per-day size cap (32 MB) writes one `cap` marker, then drops.
- **Disabled = near-zero cost:** dictation ids are still issued (ASR biasing
  and the output stages key the app context by them), but every hook returns
  before copying a fact or text.
- **Retention:** `dictation_journal_retention_days` (default 90, clamped
  1–365; the settings UI offers 30 / 90 / 180 / 365), pruned at startup, on
  day rollover and on a retention change; only `YYYY-MM-DD.jsonl` files are
  ever deleted.
- **App context** (`dictation_context`): at recording start (after overlay and
  mic start) the frontmost pid is resolved on the main thread and a background
  thread reads bundle id, app name, focused window title, focused element role
  and ≤ 500 chars before the caret (`AXStringForRange`, else the whole
  `AXValue` of fields ≤ 200k UTF-16 units). Each AX message is capped at
  150 ms. Kept in memory per dictation id (`dictation_context::get(id)`) for
  ASR biasing and the per-app style. macOS only. While secure event input is
  on, only the app identity is captured; a secure (password) field's text is
  never read and its window title is dropped — so neither reaches the ASR
  prompt or the journal.
- **Privacy:** local only, no network. Text (dictation stages, and the
  context line's window title / text before the caret) is journaled only for a
  verified non-secure field; otherwise it fails closed with `text_redacted`:
  `secure_field`, `secure_input`, `unknown_field` (no context or no focused
  element) or `unverified_platform` (no Accessibility API — every platform but
  macOS).
- **New files:** `src-tauri/src/journal/` (`mod.rs`, `dictation.rs` hooks,
  `record.rs` schema, `writer.rs`, `commands.rs`),
  `src-tauri/src/dictation_context/mod.rs`,
  `src/components/settings/DictationJournal.tsx`, `docs/journal.md`. AX
  helpers added to the fork-owned `correction_learning/ax_reader.rs`
  (`read_focus_context`, `app_identity`); the learning session (fork-owned)
  feeds the journal.
- **Upstream files touched:** `actions.rs` (`begin_dictation`,
  `record_start_path`, `mark_mic_ready`, `capture_async`, `start_failed`, the
  `DictationGuard` through stop → paste), `managers/transcription.rs`
  (`record_asr`, `record_allowlist_guard` / `record_allowlist_result`,
  `record_text_stage` in `post_process_transcription_text`, each gated on
  `journal::is_enabled()`), `transcription_coordinator.rs`
  (`journal::AUTO_STOP_TRIGGER`), `overlay.rs` (`epoch_ms` on the show log
  line), `RecordingOverlay.tsx` (show breadcrumbs →
  `commands.journalOverlayStage`), `settings.rs` (`dictation_journal_*`),
  `lib.rs` (`mod journal;`, `mod dictation_context;`, `journal::init`,
  `journal::commands::*`), `stores/settingsStore.ts`, `AdvancedSettings.tsx`,
  `bindings.ts`.
- **Debug:** `get_journal_dir_path` / `open_journal_dir` commands (the
  Advanced → History group shows the retention choice and the folder with an
  Open button).
- **Probe:** `journal: *` and `dictation-context: *` in
  `scripts/fork-check.sh`.
- **Upstream check:** does upstream now keep structured per-dictation telemetry
  or capture app context?
  ```bash
  git grep -iE "journal|telemetry|jsonl|frontmost|AXFocused|app.?context" upstream/main -- src-tauri/src
  ```
  Upstream's `history` (WAV + text) is not an equivalent. On a conflict in
  `transcription.rs`, re-apply the hooks; they are one-liners next to the
  allowlist / text-rules grafts.

## overlay: compact capsule + latency

The Minimal / transcribing / processing overlay is one fixed 144x32 capsule
(7 bars that settle into a shimmer while working; the fallback-model pass is
the only state with text), shown with no awaits before the first paint.

- **Instant show:** `show-overlay` commits every visible change inside
  `flushSync`; language + placement are read on mount and refreshed in the
  background only when the overlay appears (hidden → visible). The capsule
  stays mounted while hidden so it can fade out; `key={showSeq}` remounts it on
  each new appearance to replay the entrance animation. `actions.rs` sets the
  tray icon after the overlay (the tray sync used
  to cost 17–60 ms ahead of it).
- **Arming + accessibility:** the arming waveform pulses only in opacity to
  preserve round caps, and rests with reduced motion. Cancel reuses `tray.cancel`;
  the capsule status uses translated `overlay.recording` while recording and
  the existing work labels during processing.
- **Latency breadcrumbs:** `TranscribeAction::start` and the `overlay
'<state>'` show line in `overlay.rs` log `epoch_ms=`; the webview reports
  `show '<state>' handler|first-frame epoch_ms=…` through the journal's
  `journal_overlay_stage` command (app log + journal `overlay` events, see
  [dictation journal](#dictation-journal--app-context)).
- **New files:** none.
- **Upstream files touched:** `src/overlay/RecordingOverlay.tsx` (capsule
  markup, `flushSync` show, `showSeq` remount, breadcrumbs, recording-limit
  ring, fallback label), `src/overlay/RecordingOverlay.css` (`--ov-capsule-*`
  geometry and animations), `overlay.rs` (`OVERLAY_WIDTH` 164 + test bounds,
  `epoch_ms` on the show line), `actions.rs` (tray after overlay, `epoch_ms` on
  the start line).
- **Probe:** `overlay: *` in `scripts/fork-check.sh`.
- **Upstream check:** did upstream redesign the compact overlay or its show
  path? On a conflict in `RecordingOverlay.tsx` / `.css`, prefer upstream's
  structure and re-apply the capsule geometry only if still wanted.
  ```bash
  git diff main upstream/main -- src/overlay src-tauri/src/overlay.rs
  ```

## vad-edges: dictation keeps internal pauses

Offline VAD (the batch dictation path) trims leading silence and preserves
internal pauses up to 3 seconds. Quiet frames after the first speech are held in
a `pending_gap` buffer and appended when speech resumes; gaps longer than
3 seconds retain only their first and last 750 ms, dropping the middle. At stop,
a trailing gap beyond VAD hangover is kept whole
when it is at most 1.5 seconds; longer gaps keep only their first 700 ms. This
protects quiet final words classified as noise. Tail retention runs after all
resampler flush frames have been classified, preserving upstream's end-of-sentence
tail drain. Recordings without confirmed speech stay empty. Live VAD callbacks
(streaming) keep their filtered frames unchanged.

- **New files:** none (tests in the existing `recorder/tests.rs`).
- **Upstream files touched:** `audio_toolkit/audio/recorder.rs`
  (`handle_frame` gap logic, `CaptureProcessor::pending_gap` reset per
  recording, bounded tail retention after the final drain/flush, and buffer
  release), `recorder/tests.rs`.
- **Probe:** `vad-edges: *` in `scripts/fork-check.sh`.
- **Upstream check:** does upstream's offline VAD still drop internal noise
  frames?
  ```bash
  git show upstream/main:src-tauri/src/audio_toolkit/audio/recorder.rs | grep -n -A6 "VadFrame::Noise"
  ```
  If upstream keeps internal pauses itself, drop this patch.

## benchmark harness

Headless voice-dictation benchmark (`handy-bench` binary): corpus recorder,
transcription engine driver, scoring, denoise experiments, per-model limit probing.
Development tooling, not shipped in the app.

- **New files:** `src-tauri/src/bench/` (8 files), `src-tauri/src/bin/handy-bench.rs`,
  `bench/` (README + corpus manifest).
- **Upstream files touched:** `lib.rs` (one `pub mod bench;`), `Cargo.toml`
  (bench-only deps + `[[bin]]`).
- **Probe:** `bench: *` in `scripts/fork-check.sh`.
- **Upstream check:** none needed — purely additive dev tooling; drop only if you
  stop benchmarking.

## fork build & maintenance tooling

The fork's own scripts and docs — never conflicts with upstream.

- **Files (all fork-owned):** `scripts/build-signed-dmg.sh`,
  `scripts/sync-upstream.sh`, `scripts/fork-check.sh`, `docs/REQUIREMENTS.md`,
  `docs/fork-patches.md` (this file), the "Fork Workflow" section of `AGENTS.md`.
- **Upstream check:** none.

## ASR biasing (vocabulary + app context)

Capability-gated `RunOptions::vocabulary` / `prompt` for Qwen3-ASR (including
allowlist fallback), batch ASR, pinned retries and streaming. Custom words take
priority over active, enabled learned corrections ranked by observation count,
then recency. Terms are trimmed, deduplicated case-insensitively, limited to 40
Unicode characters each and 40 terms total. Overlong terms are omitted.

- **New files:** `src-tauri/src/asr_bias/mod.rs` (builder, capability gates,
  `run_biased`, settings + support commands, tests),
  `src/components/settings/AsrContextBiasing.tsx`.
- **Hook points:** `lib.rs` (`mod asr_bias`, command registrations in the fork
  command block), `settings.rs` (`asr_context_biasing_enabled` in the fork
  block, default true for fresh and existing stores), `bindings.ts`, and the
  four engine runs in `managers/transcription.rs`: one
  `asr_bias::run_biased(..)` call each for the allowlist fallback, the batch
  run and the pinned retry (bias + one unbiased retry if the engine rejects
  it), and `asr_bias::apply(..)` on the stream's `RunOptions`. Whisper's
  existing initial prompt is preserved; unsupported models receive empty
  vocabulary and no context prompt. UI: `general/ModelSettingsCard.tsx`
  mounts `AsrContextBiasing` (shown for every model, so the card no longer
  hides when a model has no language settings), `stores/settingsStore.ts`.
- **Context:** `journal::current_id()` joins the run to
  `dictation_context::get(id)` without waiting for async capture. Never uses a
  previous dictation's context. Prompt includes non-empty app name (120 chars),
  window title (180 chars), and the last 300 Unicode characters before the caret.
  Capture reads neither text nor window title of a secure field or while
  secure event input is on; the builder checks `secure` again.
- **Fork-owned integration:** `correction_learning::snapshot` exposes loaded
  correction pairs. `journal::record_asr_bias` appends count-only `asr_bias`
  entries in run order (`bias_vocab_n`, `bias_prompt_chars`); no prompt text is
  journaled by this hook. Each run logs only these counts at debug level.
- **Setting:** `change_asr_context_biasing_setting(enabled)` persists the shared
  vocabulary/context switch; `get_asr_bias_support(model_id)` tells the UI
  whether the model accepted a bias on its last run.
- **Probe:** `asr-bias: *` in `scripts/fork-check.sh`.
- **Upstream check:** did upstream wire vocabulary, learned corrections, or
  app context into capability-gated ASR options?
  ```bash
  git grep -iE "Feature::Vocabulary|Feature::ContextPrompt|asr.?bias|text_before_caret" upstream/main -- src-tauri/src
  ```
  Drop the module, settings/binding/command hooks, journal count hook and probes
  together if upstream ships equivalent behavior. Watch all four run-option
  sites when absorbing transcription pipeline changes.
