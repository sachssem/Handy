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
| [snippets](#snippets)                                                                    | feature | low            | `managers/transcription.rs`, `settings.rs`, `lib.rs`, settings UI                                                                                     |
| [self-correction LLM pass](#self-correction-llm-pass)                                    | feature | low            | `actions.rs`, `settings.rs`, `lib.rs`, `build.rs`, settings UI                                                                                        |
| [per-app styles](#per-app-styles)                                                        | feature | low            | `actions.rs` (shared hook), `settings.rs`, `lib.rs`, settings UI                                                                                      |
| [overlay: compact capsule + latency](#overlay-compact-capsule--latency)                  | feature | medium         | `RecordingOverlay.tsx`, `RecordingOverlay.css`, `overlay.rs`, `actions.rs`                                                                            |
| [vad-edges: dictation keeps internal pauses](#vad-edges-dictation-keeps-internal-pauses) | fix     | medium         | `audio_toolkit/audio/recorder.rs` (+ its `tests.rs`)                                                                                                  |
| [history: pasted text](#history-pasted-text)                                             | fix     | low            | `actions.rs`                                                                                                                                          |
| [paste last transcript hotkey](#paste-last-transcript-hotkey)                            | feature | low            | `settings.rs`, `actions.rs`, `tray.rs`, `lib.rs`, `GeneralSettings.tsx`                                                                               |
| [fork never self-updates](#fork-never-self-updates)                                      | fix     | very low       | `settings.rs` (one early return)                                                                                                                      |
| [model warm-up after load](#model-warm-up-after-load)                                    | perf    | low            | `managers/transcription.rs`, `lib.rs`                                                                                                                 |
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
  In a German utterance an English `dot` over plain spaces joins any non-veto
  words verbatim (`Discount dot value` → `Discount.value`).
  Ticket ids `ACRONYM dash|Bindestrich|minus 123` → `PP-106` in any language; in a
  German utterance English `w dash w` → `w-w` (keys `dash` / `Bindestrich`).
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
- **ITN** (`itn.rs`): English number words combine only by cardinal grammar
  (`twenty one` → 21, `one hundred twenty three` → 123); separate numbers back to
  back (`fifty fifty`, `five six`, `nine eleven`) stay words, never summed. English
  number words are not converted when the utterance language is German (the
  pipeline passes the output-language evidence into `apply_text_rules`).
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
dismissing the toast (`⌃esc` chip or `ctrl+escape`) changes nothing — a suggestion stays
a suggestion and can still be blocked from the settings list;
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
  It also rejects any deletion-only or single-char-append edit of a common
  word (`Werkstatt → Werk`), whatever the result. Edits diffed against a
  paste-only anchor (the paste was never found verbatim) use at most the
  Conservative distance bound and always need the phonetic match. The store
  refuses a candidate whose misheard side is the intended side of a known pair
  (`chained`, e.g. `X High → xhigh` then `xhigh → high`); a second target for
  the same misheard text replaces the first, so one session never keeps two.
  First names are known and distinctive case-insensitively, even when also
  common words (`mark → marc`, `Mark → Marc`); case-only edits remain rejected.
  Field diffs split `@`, `.`, `/` into separate tokens, so e-mail local-part
  and URL corrections yield individual word pairs without losing byte offsets.
- **Settling:** a candidate set commits only after the field was quiet for
  `SETTLE` (1.5 s) — every text change _and_ every caret move restarts it
  (`AXSelectedTextChanged` wakes the session), and a superseded intermediate
  set never settles, so a multi-step fix (`Sparaboos` → `Spar-Abos`) is
  proposed once, as its final state. With the caret at a word end it waits
  `LONG_SETTLE` (3 s). Clear-on-submit / focus change still commit a finished
  edit at once; the window expiring drops a set younger than `SETTLE`.
- **Toast focus:** the toast panel never becomes key
  (`can_become_key_window: false`, `focusable(false)`) and is revealed via the
  nspanel API (`orderFrontRegardless`), never `WebviewWindow::show` (tao's
  `makeKeyAndOrderFront:`) — otherwise it captured the keyboard and e.g. a
  Return meant for a chat composer was lost. Clicks still work through
  `accept_first_mouse(true)` + the non-activating style mask.
- **Toast shortcuts:** Accept (`learned_toast_accept_shortcut`, default
  `ctrl+enter`, plus the keypad-Enter twin; only with suggestions), Undo
  (`learned_toast_dismiss_shortcut` — the name predates the re-scope —
  default `ctrl+backspace`; only with learned pairs) and a fixed plain dismiss
  (`ctrl+escape`, not bare Esc, so Esc keeps reaching the focused app; the
  corner chip shows it as `⌃esc`) are registered only while a toast is visible (`toast_shortcuts.rs`: armed on reveal,
  disarmed on every hide, generation-guarded take-once claim), through the
  active keyboard backend. Each reveal carries its own event into the main-thread
  closure; delayed hides match the current toast id/generation, and
  `hide_learned_toast(id: Option<String>)` also supports legacy unscoped calls.
  Reconciliation uses blocking workers. Shortcut capture suspends transient
  registrations and claims, restoring only the current visible toast afterwards.
  Bare/Shift-only combos return `needs-modifier`; conflicts retain the
  `conflict:<binding_id>` marker. Editable under "Learning options"
  (`LearnedToastShortcutInput.tsx`); the toast buttons show them as key hints.
  The fixed dismiss combo is reserved against Accept / Undo. Cancel is only
  registered while recording; if a recording starts with the toast visible and
  the cancel binding resolves to `ctrl+escape` (side-agnostic), dismiss yields
  (the backends refuse duplicates): the cancel reconciliation releases it
  before registering cancel and hands it back after unregistering.
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
    shortcuts in settings shortcut capture; `cancel_shortcut_requested()` and
    the cancel reconciliation's `yield_dismiss_to_cancel` /
    `reclaim_dismiss_from_cancel` calls hand `ctrl+escape` between the toast
    and the recording cancel shortcut.
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

## snippets

Spoken trigger phrase → user-defined text (Wispr "snippets"), e.g. "meine
Adresse" → a postal address, "mein Calendly" → a link.

- **Semantics** (`snippets/mod.rs`): fires only as a delimited phrase — the
  trigger's words fill a whole clause, preceded by the text edge, a comma or
  `. : ! ?` / newline and followed by the text edge or `. : ! ?` / newline.
  A following comma never qualifies (relative clauses stay prose).
  ("meine Adresse." / "Hallo Marc, mein Calendly." fire;
  "Schick das an meine Adresse" does not). Case- and punctuation-insensitive on
  words ("Meine, Adresse." matches); ASR punctuation around a trigger at a text
  edge is dropped; longest trigger first; disabled snippets never fire. Spoken
  punctuation commands ("Komma") are not delimiters — snippets run before the
  text rules.
- **Protection:** stage between `normalized` and the text rules. A fired
  trigger becomes a private-use placeholder (`U+E000…`, no word class) so text
  rules and learned corrections never touch the expansion; it is restored
  right after `apply_learned`, inside `post_process_transcription_text`, so no
  placeholder ever leaves the transcription (history, bench, streaming). The
  first restoration records actual fired state and byte spans in a small
  bounded dictation-id-keyed slot inside `snippets`; intermediate journal restorations
  cannot overwrite it. `output_stages` consumes it once, skips self-correction
  when a trigger fired, and protects actual inserted spans from app styling.
  Coincidental expansion text has no protection; changed output never reuses
  stale offsets.
- **Settings / commands:** `snippets: Vec<Snippet{id, trigger, expansion,
enabled}>`; granular `add_snippet`, `update_snippet`, `remove_snippet`
  (validated: ≥ 1 word, non-empty expansion, unique trigger words; each emits
  `settings-changed`).
- **New files:** `src-tauri/src/snippets/` (`mod.rs`, `commands.rs`),
  `src/components/settings/SmartFormatting.tsx` (shared with the two features
  below).
- **Upstream files touched:** `managers/transcription.rs` (`snippets::shield`
  before `apply_text_rules`, `snippets.restore` after `apply_learned`, journal
  stage), `settings.rs` (`snippets`), `lib.rs` (`mod snippets;`, commands),
  `stores/settingsStore.ts`, `AdvancedSettings.tsx` (mounts `SmartFormatting`),
  `bindings.ts`.
- **Probe:** `snippets: *` in `scripts/fork-check.sh`.
- **Upstream check:** does upstream now expand text shortcuts / snippets?
  ```bash
  git grep -iE "snippet|text.?expan|shortcut.?phrase" upstream/main -- src-tauri/src src
  ```

## self-correction LLM pass

Trigger-gated LLM disfluency cleanup (Wispr Flow's "backtrack" plus filler
cleanup): "um drei, nein warte, um vier" → "um vier", "morgen, äh,
übermorgen" → "übermorgen", "ist ist" → "ist". Slot-only repairs run
deterministically before provider selection; the LLM runs **only** when the
transcript holds a detected disfluency the rules cannot fully repair. Everything else is pasted as the rules
produced it (no latency, no over-editing). Controlled by
`self_correction_llm_enabled` (default on). Remote providers additionally
require upstream's `post_process_enabled`; without that consent, only
on-device Apple Intelligence may run.

- **Slot rules** (`self_correction/rules.rs`): a detected cue/restart whose
  retracted tail is exactly one number, weekday, month, relative day or known
  first name is replaced by the immediate same-class slot after the cue.
  An omitted preposition stays (`at five, make that six` → `at six`);
  a repeated preposition/article frame must match exactly and is retained
  from the replacement (`um drei, nein warte, um vier` → `um vier`). The
  old slot, cue and boundary punctuation are removed; replacement casing is
  adjusted at the cut, final punctuation is preserved. The existing guard
  also checks the rules result. If every detected span is repaired, provider
  selection/inference is skipped (`provider: rules`, `reason: applied`).
  Otherwise the LLM sees the whole original text with its original spans.
  Lists, questions, estimates, mismatched frames, clock times split by a
  colon and complex retractions stay outside these conservative rules.
  Disabled, snippet and prior-post-processing gates apply to rules too.
- **Detection** (`self_correction/disfluency.rs`, on the final text — i.e.
  **after** upstream's deterministic filler removal and 3+-stutter collapse,
  so a dictation whose only disfluency upstream already removed costs no LLM
  call). Each finding is a span; journal kind in brackets:
  - `filler`: hesitation sounds upstream left in (`äh`, `ähm`, `öhm`, `hm`,
    `mhm`, `uh`, `uhm`, …; `um` / `er` / `erm` only for English — German
    words otherwise). A filler between restating clauses also licenses the
    retraction ("morgen, äh, übermorgen").
  - `repetition`: an immediate word or two-word repeat without a pause between
    ("ich ich", "the the", "ich bin ich bin"); never legitimate doubles
    (`das/die/der/den/dem/des`, `sie`, `that`, `had`, `is`, intensifiers and
    interjections like `sehr`, `ja`, `very`).
  - `restart`: a dash / ellipsis whose next clause restates the previous one
    or restarts a fragment of at most two words with one of its words
    ("Ich wollte – ich muss los"); or a comma between restating clauses
    ("morgen, übermorgen", "am Montag, am Dienstag", "den roten Stift, den
    blauen Stift"). **Design decision:** upstream's filler removal turns
    "morgen, äh, übermorgen" into "morgen, übermorgen" before this stage and
    the pre-filler text is not passed here (that would need another graft in
    `managers/transcription.rs`), so the comma juxtaposition itself is the
    signal. Qwen can also drop the comma entirely: two adjacent, different
    weekday / month / relative-day slots then retract the first (`morgen
übermorgen`); adjacent numbers (`drei vier Tage`) and names (`Anna Maria`)
    stay untouched. Without a repeated frame word only weekday / month / relative-day
    slots count (numbers: "drei, vier Tage" is an estimate; names: "Danke
    Anna, Paul …" is an address); a conjunction after it (`und`, `oder`,
    `bis`, `and`, `or`, `to`, …) or a third comma item means a list.
  - Restatement ([`cues::restates`]): the first replacement word shares a
    slot class (number/time, weekday, month, relative day, first name) with a
    word in the immediately preceding clause, even if that slot is not the
    clause's final word. Name slots use the shared lexicon (including its
    supplemental `Lena` entry). The nearest matching slot starts the retraction.
    Alternatively, the first replacement word repeats one of the last three
    clause words (frame), followed within two words by a matching slot or
    repeated head noun (`Ruf Anna an. Sorry, ruf Lena an.`). Frameless comma
    restarts still exclude numbers/names; `Guten Morgen` is never a day slot.
  - `cue:<words>` (`self_correction/cues.rs`, word boundaries,
    case-insensitive, pauses between cue words skipped). Every cue needs
    preceding dictation and, except the bounded inline-wait and inline
    alternative rules below, a
    pause directly before it (including `.`, `,`, `;`, dash, ellipsis or a
    newline). A question mark before or inside any cue vetoes detection:
    `Ist es Montag? Nein, Dienstag.` remains question/answer.
    - Pause before suffices for existing explicit cues: `nein warte`,
      `nee warte`, `nein moment`, `nein/sorry ich meine|meinte`, `no wait`,
      `no/sorry I mean|meant`.
    - Existing ordinary phrases require pauses before and after:
      `streich(e) das`, `vergiss das`, `ich meinte`, `wait no`, `actually no`,
      `scratch that`, `strike that`, `I meant`.
    - Correction verbs/phrases need an immediate restatement **or** pauses
      on both sides: `korrigiere`, `korrigier`, `Korrektur`, `ich korrigiere`,
      `berichtige`, `besser gesagt`, `genauer gesagt`, `oder besser`,
      `oder vielmehr`, `correction`, `make that`, `let me rephrase`. A
      delimited cue without a matching slot retracts only its preceding
      clause. `Ich komme morgen. Korrigiere übermorgen.` now triggers.
    - Bare negations `nein`, `nee`, `ne`, `nö`, `no`, `nope`, `nah`, and
      ambiguous `vielmehr`, `beziehungsweise` / `bzw.`, `sorry`,
      `Entschuldigung`, `pardon`, `rather`, `or rather` **always** require an
      immediate repeated-frame or same-class-slot restatement. This catches
      `Ich komme morgen. Ne, übermorgen.` and
      `Treffen am Montag, beziehungsweise Dienstag.` without triggering
      `Ne, das passt schon.`, `Nein danke.`, `Das ist gut. Nein, wirklich.`,
      `Ich nehme Tee bzw. Kaffee.`, `Sorry, ich bin spät dran.`,
      `Korrigiere bitte den Text.`, `Besser gesagt ist das nicht.` or
      `Rather than waiting, we go now.`. `naja` is not a correction cue.
      `beziehungsweise` / `bzw.` also count without preceding punctuation
      when directly between different slots of the same class:
      `Treffen am Montag beziehungsweise Dienstag.` → `Treffen am Dienstag.`.
      The retraction then includes the word directly before the cue;
      `Tee bzw. Kaffee` still has no slot. A multiword cue owns its words
      (`nein warte` never also produces an overlapping `warte` span).
    - `ich meine` / `I mean` keep their existing frequent-filler rule:
      pauses before and after **and** a number, weekday/month or known
      first name within three following words, or a restatement. Weak
      anchors exclude `one`, `may`, `march`, `today`, `heute`, `morgen`.
      Reported speech (`said` / `asked` / `sagte` / `fragte` nearby, or colon
      plus opening quote directly before the cue) never triggers.
    - Bare `Warte` / `Moment` / `Wait` after a clause boundary (`. , ;` /
      dash / newline): an anchor within three replacement words and an
      anchor of the same kind in the immediately preceding clause, or a
      restatement. No dictation-start cue, colon introduction, reported
      speech, unrelated earlier anchor, or anchor in a later clause. This
      covers ASR dropping `nein`: `Wir treffen uns um drei. Warte um vier.`
      Inside a clause, these cues also count when a number/time, weekday,
      month or relative-day slot in the next two words matches a slot in
      the preceding six words of the same sentence. A following pronoun
      vetoes the inline rule. The retraction starts at the earlier slot's
      repeated frame word (or the slot): `um drei und dann warte um vier`
      → `um vier`. Ordinary waits remain unchanged.
- **Prompt:** minimal-edit, deletion-only cleanup (fillers, stutters, aborted
  starts, retracted part + cue; keep every other word, punctuation, casing,
  language; keep lists/estimates; return unchanged if nothing to clean) with
  DE/EN few-shot examples incl. a no-change one and explicit `Ne,`,
  `Korrigiere` and `beziehungsweise` repairs (both simple slots and internal
  slots with retained trailing words for the LLM fallback)
  (`self_correction::SYSTEM_PROMPT`).
- **Provider:** Apple Intelligence on-device when available; else the active
  post-process provider only with upstream post-processing enabled, a model
  **and** an API key; otherwise skipped (`no_local_provider` without remote
  consent, or `no_provider` / `apple_unavailable`). Apple availability and
  inference run on blocking workers; at most one Apple call is in flight.
  A timeout keeps rules output and discards late results; until Swift
  generation actually unwinds, later calls skip with `busy`, even after the
  blocking worker returned. Preparation is deferred during generation; a run
  also skips `busy` while preparation is already creating a session.
- **Apple session** (`swift/fork_self_correction.swift`, FFI in
  `self_correction/apple_session.rs`): the pass does **not** use upstream's
  per-call bridge (fresh session, structured output with a silent unstructured
  retry, uncancellable). It keeps one `LanguageModelSession` with
  `SYSTEM_PROMPT` as instructions, `prewarm()`ed ahead of use: `prepare` runs
  at every recording start (`TranscribeAction::start`, after the mic started;
  non-blocking, idempotent while fresh, only when the pass is enabled and
  `choose_provider` would pick Apple). Ready sessions older than 90 s are
  released and prewarmed again: idle age cannot prove the model stayed resident
  under memory pressure. No app-start warm-up. A run takes the ready session (or
  builds a cold one), generates plain text greedily with
  `maximumResponseTokens = max(48, chars/2)` (≈ 2× input tokens), cancels the
  Swift task at its deadline and returns `timeout` at once; once the task
  unwound, a fresh session is prewarmed so no transcript history accumulates.
  Budget: 3 s when the session was prewarmed 1.5–90 s ago (`WARM_AFTER` /
  `WARM_FRESH_FOR`, an estimate — `prewarm()` has no completion signal), 4.5 s otherwise
  (`COLD_TIMEOUT`). Debug log: `Apple session prepared N ms ago → warm|cold`
  and `Apple run N ms (cap N ms)`. Built by `swift/fork_self_correction_build.rs`
  (same real/stub decision and swiftc flags as upstream's bridge;
  `fork_self_correction_stub.swift` reports unavailable).
- **Guard** (`self_correction/guard.rs`, word alignment plus verbatim retention): result length must be
  30–110 % of the input; **no** added word (word-level diff, not even a
  re-inflected one); every removed word lies in a span's deletable words
  (filler, repeated words, cue) or in a retracted stretch that starts no
  earlier than the repair's start (explicit cues: anywhere before; bare-no
  and inline-wait repairs: the matched slot/frame; restart: the frame word /
  fragment start) and ends at or within two words before the repair. Replacement
  words after the cue cannot be deleted.
  A sentence boundary before a negation does not shift the matched-slot
  start: `Ich komme morgen. Ne, übermorgen.` retracts from word 2 (`morgen`),
  not from the new clause. Regression tests accept the correct output with
  a period, no final punctuation or an exclamation mark, while still
  rejecting inserted words and unrelated deletions.
  Retained text is byte-identical outside cuts: numbers, symbols, punctuation
  and casing elsewhere cannot change. Only cut whitespace, one adjacent
  comma/period/dash on either side, and the first letter after a cut or at text
  start may change. A final mark on a slot immediately following a cut may
  be dropped or become a sentence-final mark. Wrapping quotes /
  `<think>` / `Output:` labels are stripped. A failed guard keeps the rules
  output; reason journaled. Skipped when a snippet fired or upstream's LLM
  already rewrote it.
- **Journal:** `self_correction {requested, applied, ms, reason, cue,
provider, candidate?}` (only when a disfluency was found; `cue` = the distinct detected
  kinds, e.g. `cue:nein warte,filler,repetition`) and `text.self_correction`.
  `candidate` holds the cleaned LLM result when rejected, including
  `unchanged`; accepted results use `text.self_correction`. Candidate text
  is local-journal-only, never logged, and omitted under the journal's
  existing fail-closed text redaction.
  Debug log: `self-correction: detected '<kinds>' in N µs` and the outcome
  line with ms.
- **New files:** `src-tauri/src/self_correction/` (`mod.rs`, `cues.rs`,
  `disfluency.rs`, `rules.rs`, `guard.rs`, `blocking.rs`, `commands.rs`,
  `apple_session.rs`), `src-tauri/src/output_stages.rs` (sequences this
  pass and the per-app style), `src-tauri/swift/fork_self_correction.swift`
  with its `_stub.swift`, `_bridge.h` and `_build.rs` siblings. `correction_learning::lexicon` became
  `pub(crate)` (first-name list).
- **Upstream files touched:** `actions.rs` (one wrapped call:
  `crate::output_stages::finish(&ah, process_transcription_output(..))` in
  `TranscribeAction::stop` — live dictations only, not history retries;
  `crate::self_correction::prepare(&settings)` in `TranscribeAction::start`),
  `settings.rs` (`self_correction_llm_enabled`), `lib.rs` (`mod
self_correction;`, `mod output_stages;`, command),
  `build.rs` (`#[path] mod fork_self_correction_build;` +
  `fork_self_correction_build::build()` after the upstream bridge),
  `stores/settingsStore.ts`, `bindings.ts`.
- **Probe:** `self-correction: *` in `scripts/fork-check.sh`.
- **Upstream check:** does upstream now detect self-corrections / disfluencies
  or gate the LLM pass on content? Does its filler removal now cover
  restarts (then drop the `restart` comma heuristic)? Does upstream's
  `apple_intelligence.swift` now reuse/prewarm a session and cancel on
  timeout (then drop the fork Swift bridge and call upstream's)?
  ```bash
  git grep -iE "self.?correct|backtrack|scratch that|no wait" upstream/main -- src-tauri/src
  ```

## per-app styles

Deterministic output style per target app (bundle id from `dictation_context`,
captured at recording start), the last text stage before the paste.
`app_styles_enabled` (default on) + `app_styles_categories {chat, terminal,
code, match_context}` (all default on). Unknown apps and mail/docs (Mail,
Outlook, Notes, Pages, Word) are unchanged; browsers are not classified.

- **chat** (Slack, Messages, WhatsApp, Telegram, Discord, Signal, Teams): a
  one-sentence, one-line message drops a single trailing `.`; `?`/`!`/`...`
  stay.
- **terminal** (Terminal, iTerm2, Warp, Ghostty, kitty, Alacritty, WezTerm):
  trailing `.` dropped; a leading function word is lowercased; newlines kept
  (TUIs such as Claude Code take multi-line input, shells get a bracketed
  paste); no context matching (terminal AX text is the screen buffer).
- **code** (VS Code, Cursor, Zed, JetBrains `com.jetbrains.*`, Xcode, Sublime,
  Nova, Android Studio): a single whitespace-free token drops its trailing `.`.
- **match_context** (all but terminals): when the text before the caret ends
  mid-sentence (last char letter/digit/`,`/`;` on the same line), the first
  word is lowercased **only** if it is a known DE/EN function word (list per
  detected language — `correction_learning::last_transcription_language` —
  else both; German nouns and formal `Sie`/`Ihr` are never lowercased;
  acronyms, inner capitals, `I`, custom/learned dictionary words stay). At a
  sentence start — caret at the field start (capture read the caret but no
  text before it), blank text, or `. ? ! …` (+ closing quote/bracket) followed
  by whitespace; not after `z. B.`/`e.g.`/ordinals — a plain lowercase first
  word is capitalised (not in code editors; technical tokens, words with
  capitals and lowercase dictionary words stay). A
  space is prepended when the caret follows a word or `, . ; : ! ? ) ] }`
  directly and the dictation starts with a word (upstream only appends a
  trailing space, so no double spaces).
- **Journal:** `text.app_style` (text after the stage).
- **New files:** `src-tauri/src/app_styles/` (`mod.rs`, `commands.rs`).
- **Upstream files touched:** shares the `actions.rs` hook with the
  self-correction pass; `settings.rs` (`app_styles_*`), `lib.rs` (`mod
app_styles;`, commands), `stores/settingsStore.ts`, `bindings.ts`.
- **Probe:** `app-styles: *` in `scripts/fork-check.sh`.
- **Upstream check:** does upstream now adapt output per app?
  ```bash
  git grep -iE "bundle.?id|per.?app|app.?style|frontmost" upstream/main -- src-tauri/src
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

## history: pasted text

A history entry's processed field holds the text that was actually pasted
(after `output_stages`: self-correction, per-app style) whenever it differs
from the raw transcription; otherwise upstream's LLM result as before. The raw
transcription stays unchanged.

- **New files:** none.
- **Upstream files touched:** `actions.rs` (`pasted_text` passed to
  `hm.save_entry` in `TranscribeAction::stop`).
- **Probe:** `history: pasted text in processed field` in
  `scripts/fork-check.sh`.
- **Upstream check:** does upstream now store the final pasted text in history
  itself?
  ```bash
  git grep -n "save_entry" upstream/main -- src-tauri/src/actions.rs
  ```

## paste last transcript hotkey

A configurable global hotkey (`paste_last_transcript`, default **Ctrl+V** on
macOS, Alt+Shift+V elsewhere — Ctrl+V is the system paste there) re-pastes the
most recent transcription into the focused app. Text source is the tray's "Copy
last transcript" selection (`get_latest_completed_entry`, processed field —
the pasted text, see [history: pasted text](#history-pasted-text) — else raw),
pasted verbatim through `clipboard::paste`, so paste method, delays, clipboard
restore, trailing space and auto-submit match a dictation paste. Fires on press,
only while idle (not recording / transcribing), once until release and at least
600 ms apart to suppress auto-repeat; no history → no-op with a debug
log. Ends a running correction-learning session before pasting and opens none,
writes no journal record. Existing stores get the binding through upstream's
"merge missing default bindings" load path.

- **New files:** `src-tauri/src/paste_last.rs` (action, default binding, text
  selection, idle gate, tests).
- **Upstream files touched:** `settings.rs` (default binding in
  `get_default_settings`), `actions.rs` (`ACTION_MAP` entry), `tray.rs`
  (`last_transcript_text` made `pub(crate)`, `current_tray_state` getter),
  `lib.rs` (`mod paste_last;`), `GeneralSettings.tsx` (`ShortcutInput` row),
  locale `settings.general.shortcut.bindings.paste_last_transcript`.
- **Probe:** `paste-last: *` in `scripts/fork-check.sh`.
- **Upstream check:** does upstream ship a paste-last / re-paste shortcut?
  ```bash
  git grep -n -i "paste_last\|repaste\|last_transcript" upstream/main -- src-tauri/src/actions.rs src-tauri/src/settings.rs
  ```
  If so, drop this feature and migrate the user's binding to upstream's id.

## model warm-up after load

Experimental. Right after a transcribe-cpp model load (`load_model_with_device`,
so the press-path background load, a model switch and `--transcribe-file`
alike) one throwaway inference on 1 s of silence runs before
`loading_completed` is emitted. The first real run after a load otherwise pays
the lazy Metal pipeline compiles and weight page-in. The warm-up runs inside
the loading window (`is_loading` set) with the engine mutex held, so a real
transcription waits for it and never runs concurrently on the engine; a stop
right after the press waits at most the warm-up (~0.1–0.4 s). On the press
path (`model_unload_timeout` reload) it overlaps the user's speech. Its text
is discarded; the duration logs at debug (`Model warm-up inference took N ms`)
and lands on the dictation line as `model_warmup_ms` (with `model_load_ms`, see
[journal](journal.md)). ONNX engines are skipped (onnxruntime plans at session
creation). Kill switch: `HANDY_MODEL_WARMUP=0`.

Measured (Qwen3-ASR-1.7B Q5_K_M, Metal, 43 s WAV, `--transcribe-file
--repeat 2`, 3 runs each): first run without warm-up 3355/3460/3570 ms vs.
warm 2887/3004/2963 ms; with warm-up the first run is 3018/2992/3190 ms, the
warm-up itself 164–382 ms.

- **New files:** `src-tauri/src/model_warmup.rs`.
- **Upstream files touched:** `managers/transcription.rs` (`warm_up_engine`
  call in `load_model_with_device`, the method itself, journal hook in
  `initiate_model_load`), `lib.rs` (`mod model_warmup;`). Fork-owned:
  `journal/` (`record_model_load`, `model_load_ms` / `model_warmup_ms`).
- **Probe:** `model-warmup: *` in `scripts/fork-check.sh`.
- **Upstream check:** does upstream warm the engine after load?
  ```bash
  git grep -n -i "warm" upstream/main -- src-tauri/src/managers/transcription.rs
  ```
  If so, drop this feature.

## benchmark harness

Headless voice-dictation benchmark (`handy-bench` binary): corpus recorder,
transcription engine driver, scoring, denoise experiments, per-model limit probing.
Development tooling, not shipped in the app.

The real-dictation regression corpus is `bench/corpus/regression/manifest.toml`
(13 imported local WAVs, gitignored; stable IDs/tags + source WAV, date, history ID).
From the repo root:

```bash
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo run --manifest-path src-tauri/Cargo.toml \
  --bin handy-bench -- regression
```

Defaults to the user's selected cached model (Qwen3-ASR 1.7B Q8_0 at import),
real settings, and the **app's deterministic output function**, including
snippets, text rules/ITN, and active learned corrections. The headless driver
preloads a memory-only Tauri settings store for the learned snapshot; its
serializer rejects writes. Model lookup uses hf-hub's own cache API, without
downloads. `--model [engine:]path` overrides the model; `--no-fail` reports known
failures without failing the gate. Exact/normalized output matches and raw-ASR
spoken WER appear in the compact table and ignored JSON/MD reports. Normalized
matches are diagnostic, with an explicit path-case soft tolerance only.
Self-correction LLM cases (`needs_llm = true`) are reported separately; app
styles and the optional post-process provider are excluded because they need
app/provider context. `status = "pending_truth"` / `target = "TODO"` cases are
not scored. See `bench/README.md` for the manifest and all flags.

- **New files:** `src-tauri/src/bench/` (including `regression.rs`), `src-tauri/src/bin/handy-bench.rs`,
  `bench/` (README + corpus manifest).
- **Upstream files touched:** `lib.rs` (one `pub mod bench;`), `Cargo.toml`
  (bench-only deps + `[[bin]]`).
- **Probe:** `bench: *` in `scripts/fork-check.sh`.
- **Upstream check:** none needed — purely additive dev tooling; drop only if you
  stop benchmarking.

## fork never self-updates

Upstream's in-app updater (`tauri_plugin_updater`, official `cjpais/Handy`
release feed) would replace the fork build with an official release. The fork
forces upstream's existing `update_checks_forced_disabled()` switch (the one
`HANDY_DISABLE_UPDATER` drives for Nix) to `true`, so every existing gate goes
inert without touching the persisted `update_checks_enabled` setting: the tray
hides "Check for updates", `trigger_update_check` and the tray action no-op,
`change_update_checks_setting` refuses, and the frontend `UpdateChecker` /
`UpdateChecksToggle` read `is_update_checks_locked` and never call `check()`.
The toggle shows upstream's "Disabled by system configuration" copy.

- **New files:** none.
- **Upstream files touched:** `settings.rs` (`FORK_NEVER_SELF_UPDATES` const +
  early return in `update_checks_forced_disabled`).
- **Probe:** `updater: *` in `scripts/fork-check.sh`.
- **Upstream check:** none — the fork must never accept upstream release
  binaries. Drop only if the fork ever ships its own update feed.

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
