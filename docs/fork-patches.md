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
| [correction learning](#correction-learning)                          | feature | medium         | `shortcut/mod.rs`, `settings.rs`, `lib.rs`, `clipboard.rs`, overlay, settings UI      |
| [language-allowlist guard](#language-allowlist-guard)                | feature | medium         | `managers/transcription.rs`, `shortcut/mod.rs`, `settings.rs`, `LanguageSelector.tsx` |
| [recording-limit auto-stop](#recording-limit-auto-stop)                                  | feature | low            | `settings.rs`, `shortcut/mod.rs`, `transcription_coordinator.rs`, `managers/model.rs`, settings UI                                                    |
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

- **New files:** `src-tauri/src/text_rules/` (`mod.rs`, `itn.rs`, `substitutions.rs`),
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

Learns user corrections: after a paste, reads the focused field via macOS
Accessibility, diffs against what was pasted, and offers to remember the fix
(applied on future transcriptions), with a trial-mode toast + undo.

- **New files:** `src-tauri/src/correction_learning/` (`mod.rs`, `ax_reader.rs`,
  `differ.rs`, `session.rs`, `store.rs`, `toast.rs`),
  `src/components/settings/LearnedCorrections.tsx`, `src/toast/*`,
  `src/components/icons/RemoveIcon.tsx`, `docs/design/auto-learn-corrections.md`.
- **Upstream files touched:** `lib.rs` (`mod correction_learning;`, toast init,
  command exports, specta events), `shortcut/mod.rs` (learn/apply commands, paste
  flow hook), `settings.rs` (`learned_corrections` + aggressiveness fields),
  `clipboard.rs` (post-paste field capture), `actions.rs`, `overlay.rs`,
  `RecordingOverlay.tsx`, `AdvancedSettings.tsx`.
- **Probe:** `correction-learning: *` in `scripts/fork-check.sh`.
- **Upstream check:** does upstream ship any "learn from edits" / dictionary /
  custom-replacement feature?
  ```bash
  git grep -iE "learn|correction|replacement|dictionary|vocab" upstream/main -- src-tauri/src src
  ```
  Unlikely (upstream is frozen). This is the most invasive feature — if a rebase
  conflicts here, adapt carefully rather than dropping; verify with
  `handy --debug-toast` (see AGENTS.md "Fork Debug Helpers").

## language-allowlist guard

Constrains "auto" language detection to an allowlist; when the detected language
falls outside it, retries pinned to the first allowlisted language and can escalate
to a fallback model.

- **New files:** none (logic lives inside the touched files); reworked
  `src/components/settings/LanguageSelector.tsx`.
- **Upstream files touched:** `managers/transcription.rs` (allowlist guard +
  `run_allowlist_fallback` around the detection stage), `shortcut/mod.rs`
  (`change_language_allowlist_setting`, fallback-model setter), `settings.rs`
  (`language_allowlist`, `language_allowlist_fallback_model`).
- **Probe:** `language-allowlist: *` in `scripts/fork-check.sh`.
- **Upstream check:** did upstream add language pinning / detection constraints?
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
  recording session), `managers/model.rs` (per-model limit metadata),
  `AdvancedSettings.tsx`.
- **Probe:** `recording-limit: *` in `scripts/fork-check.sh` (incl. the
  `schedule_recording_limit` enforcement hook in `transcription_coordinator.rs`).
- **Upstream check:** did upstream add a max-recording-length / auto-stop?
  ```bash
  git grep -iE "max.?record|recording.?limit|auto.?stop|duration.?limit" upstream/main -- src-tauri/src
  ```

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
