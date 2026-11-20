#!/usr/bin/env bash
#
# fork(voice-control): mechanical integrity gate for the fork's patch set.
#
# This is the fork's equivalent of a test that asserts "every fork feature is
# still wired into the upstream code". It does NOT test behaviour; it proves that
# each feature's integration hook and its `fork(voice-control)` marker still exist
# in the upstream-owned files they were grafted into. Its whole job is to catch a
# rebase (or a careless edit) that silently dropped a hook — the single most
# expensive way a long-lived fork rots.
#
# Run it:
#   - at the end of any task that touched an upstream-owned file, and
#   - before every release build (scripts/build-signed-dmg.sh runs it for you), and
#   - after every `scripts/sync-upstream.sh` run, before pushing.
#
# When you add a new fork feature: add its probe(s) below AND a row in
# docs/fork-patches.md. A feature with no probe is a feature that can vanish on
# the next rebase without anyone noticing.
#
# Exit code 0 = all probes pass. Non-zero = at least one hook is missing.
#
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# --- Probe table -------------------------------------------------------------
# One probe per line: "Feature label|relative/file/path|extended-regex".
# The probe passes when the regex matches at least once in the file.
# Keep labels aligned with the feature names in docs/fork-patches.md.
PROBES=(
  # -- text-rules engine (deterministic punctuation / ITN) --
  "text-rules: module wired in|src-tauri/src/lib.rs|^(pub )?mod text_rules;"
  "text-rules: pipeline hook|src-tauri/src/managers/transcription.rs|text_rules::apply_text_rules"
  "text-rules: settings fields|src-tauri/src/settings.rs|text_rules_enabled"
  "text-rules: settings UI mounted|src/components/settings/advanced/AdvancedSettings.tsx|TextRules"

  # -- correction learning (AX reader, differ, session, store, toast) --
  "correction-learning: module wired in|src-tauri/src/lib.rs|^(pub )?mod correction_learning;"
  "correction-learning: toast init|src-tauri/src/lib.rs|correction_learning::toast::init_learned_toast"
  "correction-learning: tauri commands registered|src-tauri/src/lib.rs|correction_learning::commands::"
  "correction-learning: init at startup|src-tauri/src/lib.rs|correction_learning::init\\(app_handle\\)"
  "correction-learning: toast shortcut hook|src-tauri/src/shortcut/handler.rs|toast_shortcuts::handle_shortcut_event"
  "correction-learning: toast dismiss yields to recording cancel|src-tauri/src/shortcut/mod.rs|toast_shortcuts::yield_dismiss_to_cancel"
  "correction-learning: toast dismiss handed back after cancel|src-tauri/src/shortcut/mod.rs|toast_shortcuts::reclaim_dismiss_from_cancel"
  "correction-learning: toast shortcut command|src-tauri/src/lib.rs|toast_shortcuts::change_learned_toast_shortcut_setting"
  "correction-learning: toast shortcut settings|src-tauri/src/settings.rs|learned_toast_accept_shortcut"
  "correction-learning: settings fields|src-tauri/src/settings.rs|learned_corrections"
  "correction-learning: settings UI mounted|src/components/settings/advanced/AdvancedSettings.tsx|LearnedCorrections"
  "correction-learning: paste session hook|src-tauri/src/actions.rs|correction_learning::begin_session"
  "correction-learning: paste read-back diagnosis|src-tauri/src/correction_learning/session.rs|paste_check::describe\\("
  "correction-learning: pipeline hook|src-tauri/src/managers/transcription.rs|correction_learning::apply_learned"

  # -- language-allowlist guard (+ fallback model) --
  "language-allowlist: guard module wired in|src-tauri/src/managers/transcription.rs|^mod allowlist;"
  "language-allowlist: transcription guard|src-tauri/src/managers/transcription.rs|allowlist::guard\\(self, &settings, outcome, audio\\)"
  "language-allowlist: guard reads the allowlist|src-tauri/src/managers/transcription/allowlist.rs|settings\.language_allowlist"
  "language-allowlist: shortcut command|src-tauri/src/shortcut/mod.rs|change_language_allowlist_setting"
  "language-allowlist: settings field|src-tauri/src/settings.rs|language_allowlist"
  "language-allowlist: settings UI mounted|src/components/settings/general/ModelSettingsCard.tsx|<LanguageAllowlist"

  # -- recording-limit auto-stop --
  "recording-limit: settings field|src-tauri/src/settings.rs|auto_stop_recording_on_limit"
  "recording-limit: settings command|src-tauri/src/shortcut/mod.rs|auto_stop_recording_on_limit"
  "recording-limit: enforcement scheduler|src-tauri/src/transcription_coordinator.rs|schedule_recording_limit"
  "recording-limit: settings UI mounted|src/components/settings/advanced/AdvancedSettings.tsx|RecordingLimitAutoStop"

  # -- overlay: instant, compact, theme-inverted pill --
  "overlay: instant show (no awaits before paint)|src/overlay/RecordingOverlay.tsx|flushSync\\(\\(\\) =>"
  "overlay: compact capsule|src/overlay/RecordingOverlay.css|--ov-capsule-w"
  "overlay: window sized for capsule|src-tauri/src/overlay.rs|OVERLAY_WIDTH: f64 = 164"

  # -- dictation keeps internal pauses (VAD trims only the edges) --
  "vad-edges: module wired in|src-tauri/src/audio_toolkit/audio/sink.rs|^mod vad_edges;"
  "vad-edges: internal noise frames held|src-tauri/src/audio_toolkit/audio/sink.rs|vad_edges::on_noise\\("
  "vad-edges: pending gap flushed on resumed speech|src-tauri/src/audio_toolkit/audio/sink.rs|vad_edges::on_speech\\("
  "vad-edges: stop tail kept at finish|src-tauri/src/audio_toolkit/audio/sink.rs|vad_edges::take_tail\\("
  "vad-edges: long stop tail capped|src-tauri/src/audio_toolkit/audio/sink/vad_edges.rs|tail\.truncate\(OFFLINE_LONG_TAIL_KEEP_SAMPLES\)"
  "vad-edges: long internal pause capped|src-tauri/src/audio_toolkit/audio/sink/vad_edges.rs|pending_gap\.len\(\) > OFFLINE_FULL_INTERNAL_GAP_MAX_SAMPLES"

  # -- dictation journal + app context capture --
  "journal: module wired in|src-tauri/src/lib.rs|^(pub )?mod journal;"
  "journal: writer init|src-tauri/src/lib.rs|journal::init\\(app_handle\\)"
  "journal: app log keeps 3 rotated files|src-tauri/src/lib.rs|RotationStrategy::KeepSome\\(3\\)"
  "journal: commands registered|src-tauri/src/lib.rs|journal::commands::change_dictation_journal_enabled_setting"
  "journal: settings fields|src-tauri/src/settings.rs|dictation_journal_enabled"
  "journal: dictation begin hook|src-tauri/src/actions.rs|crate::journal::begin_dictation"
  "journal: stop guard hook|src-tauri/src/actions.rs|crate::journal::DictationGuard::stop"
  "journal: engine facts hook|src-tauri/src/managers/transcription.rs|asr_journal::record\\(self, &settings, &outcome"
  "journal: engine facts recorded|src-tauri/src/managers/transcription/asr_journal.rs|crate::journal::record_asr\\("
  "journal: text stages hook|src-tauri/src/managers/transcription.rs|record_text_stage\\(Stage::Learned"
  "journal: allowlist guard hook|src-tauri/src/managers/transcription/allowlist.rs|crate::journal::record_allowlist_guard"
  "journal: retention UI|src/components/settings/DictationJournal.tsx|dictation_journal_retention_days"
  "journal: overlay breadcrumb command|src-tauri/src/lib.rs|journal::commands::journal_overlay_stage"
  "journal: overlay breadcrumbs reported|src/overlay/RecordingOverlay.tsx|commands\.journalOverlayStage"
  "journal: auto-stop trigger|src-tauri/src/transcription_coordinator.rs|journal::AUTO_STOP_TRIGGER"
  "journal: settings UI mounted|src/components/settings/advanced/AdvancedSettings.tsx|<DictationJournal"
  "dictation-context: module wired in|src-tauri/src/lib.rs|^(pub )?mod dictation_context;"
  "dictation-context: capture hook|src-tauri/src/actions.rs|crate::dictation_context::capture_async"

  # -- ASR biasing (vocabulary + app context for capable models) --
  "asr-bias: module wired in|src-tauri/src/lib.rs|^mod asr_bias;"
  "asr-bias: command registered|src-tauri/src/lib.rs|asr_bias::change_asr_context_biasing_setting"
  "asr-bias: setting field|src-tauri/src/settings.rs|pub asr_context_biasing_enabled: bool"
  "asr-bias: frontend command|src/bindings.ts|changeAsrContextBiasingSetting"
  "asr-bias: apply hook (every run setup: live, batch, pin retry, fallback)|src-tauri/src/managers/transcription.rs|crate::asr_bias::apply\("
  "asr-bias: unbiased retry on batch runs|src-tauri/src/managers/transcription.rs|crate::asr_bias::run_with_unbiased_retry\("
  "asr-bias: allowlist runs reuse the run setup|src-tauri/src/managers/transcription/allowlist.rs|run_setup\(&info, settings, pinned"
  "asr-bias: context prompt capability carried|src-tauri/src/engine_supervisor/protocol.rs|pub supports_context_prompt: bool"
  "asr-bias: worker reports context prompt support|src-tauri/src/engine_supervisor/worker.rs|supports_context_prompt: model\.supports\(Feature::ContextPrompt\)"
  "asr-bias: support command|src-tauri/src/lib.rs|asr_bias::get_asr_bias_support"
  "asr-bias: settings UI mounted|src/components/settings/general/ModelSettingsCard.tsx|<AsrContextBiasing"
  "asr-bias: store updater|src/stores/settingsStore.ts|asr_context_biasing_enabled:"
  "history: pasted text in processed field|src-tauri/src/actions.rs|let pasted_text = \(processed\.final_text != transcription\)"
  "asr-bias: journal counts|src-tauri/src/asr_bias/mod.rs|crate::journal::record_asr_bias"
  "language-allowlist: token script threshold|src-tauri/src/managers/transcription/allowlist.rs|MIN_TOKEN_LETTERS: usize = 3"
  "language-allowlist: fallback script predicate|src-tauri/src/managers/transcription/allowlist.rs|if script_outside_allowlist.*text, allowlist.*is_some"
  "transcribe-cpp: truncated run keeps its partial transcript|src-tauri/src/engine_supervisor/worker.rs|match truncated_partial\(e\)"

  # -- snippets / self-correction LLM pass / per-app styles --
  "snippets: module wired in|src-tauri/src/lib.rs|^mod snippets;"
  "snippets: shield before text rules|src-tauri/src/managers/transcription.rs|snippets::shield"
  "snippets: restore after learned|src-tauri/src/managers/transcription.rs|snippets\.restore\(&learned\)"
  "snippets: settings field|src-tauri/src/settings.rs|pub snippets: Vec<crate::snippets::Snippet>"
  "snippets: commands registered|src-tauri/src/lib.rs|snippets::commands::add_snippet"
  "snippets: settings UI mounted|src/components/settings/advanced/AdvancedSettings.tsx|<SmartFormatting"
  "self-correction: module wired in|src-tauri/src/lib.rs|^mod self_correction;"
  "self-correction: output stages wired in|src-tauri/src/lib.rs|^mod output_stages;"
  "self-correction: output hook|src-tauri/src/actions.rs|output_stages::finish"
  "self-correction: settings field|src-tauri/src/settings.rs|pub self_correction_llm_enabled"
  "self-correction: disfluency detector wired|src-tauri/src/self_correction/mod.rs|disfluency::detect"
  "self-correction: guard keeps retained text verbatim|src-tauri/src/self_correction/guard.rs|if !retained_text_matches\("
  "self-correction: session warm-up at recording start|src-tauri/src/actions.rs|self_correction::prepare\("
  "self-correction: Swift session bridge built|src-tauri/build.rs|fork_self_correction_build::build\(\)"
  "self-correction: Apple path uses fork bridge|src-tauri/src/self_correction/mod.rs|apple_session::run\("
  "self-correction: upstream filler removal precedes the pass|src-tauri/src/managers/transcription.rs|remove_filler_words\("
  "app-styles: module wired in|src-tauri/src/lib.rs|^mod app_styles;"
  "app-styles: settings fields|src-tauri/src/settings.rs|pub app_styles_categories"
  "app-styles: commands registered|src-tauri/src/lib.rs|app_styles::commands::"

  # -- fork never self-updates from upstream --
  "updater: fork forces updater off|src-tauri/src/settings.rs|if FORK_NEVER_SELF_UPDATES"
  "updater: switch stays on|src-tauri/src/settings.rs|const FORK_NEVER_SELF_UPDATES: bool = true;"

  # -- paste last transcript hotkey --
  "paste-last: default binding|src-tauri/src/settings.rs|crate::paste_last::BINDING_ID\.to_string\(\),"
  "paste-last: action registered|src-tauri/src/actions.rs|Arc::new\(crate::paste_last::PasteLastTranscriptAction\)"
  "paste-last: module declared|src-tauri/src/lib.rs|^mod paste_last;"
  "paste-last: auto-repeat guard|src-tauri/src/paste_last.rs|const REPASTE_COOLDOWN"
  "paste-last: shared text selection|src-tauri/src/tray.rs|pub\(crate\) fn last_transcript_text"
  "paste-last: idle gate tray getter|src-tauri/src/tray.rs|pub\(crate\) fn current_tray_state"
  "paste-last: settings row|src/components/settings/general/GeneralSettings.tsx|shortcutId=\"paste_last_transcript\""

  # -- model warm-up inference after a load --
  "model-warmup: module declared|src-tauri/src/lib.rs|^mod model_warmup;"
  "model-warmup: load hook|src-tauri/src/managers/transcription.rs|self\.warm_up_engine\(model_id\)"
  "model-warmup: journal hook|src-tauri/src/managers/transcription.rs|journal::record_model_load"

  # -- T3 Code transcription socket --
  "transcribe-socket: module declared|src-tauri/src/lib.rs|^mod transcribe_socket;"
  "transcribe-socket: started in app setup|src-tauri/src/lib.rs|transcribe_socket::start\\(&app_handle\\)"
  "transcribe-socket: refuses during a live stream|src-tauri/src/transcribe_socket.rs|tm\.is_streaming\(\)"
  "transcribe-socket: load wait|src-tauri/src/managers/transcription.rs|pub fn wait_for_model_load"
  "transcribe-socket: load journaled on the request|src-tauri/src/managers/transcription.rs|journal::with_scope\(journal_scope"

  # -- benchmark harness (bench/) --
  "bench: module wired in|src-tauri/src/lib.rs|^(pub )?mod bench;"
  "bench: binary entry point|src-tauri/src/bin/handy-bench.rs|handy_app_lib::bench::run"
)

# Upstream-owned files that carry fork edits MUST retain a `fork(voice-control)`
# marker, so a reviewer (human or AI) diffing against main can find every graft
# point. New fork-only modules do not need markers (the whole file is ours).
MARKER_FILES=(
  "src-tauri/src/lib.rs"
  "src-tauri/src/settings.rs"
  "src-tauri/src/shortcut/mod.rs"
  "src-tauri/src/managers/transcription.rs"
  "src-tauri/src/transcription_coordinator.rs"
  "src/stores/settingsStore.ts"
  "src/overlay/RecordingOverlay.tsx"
  "src-tauri/src/actions.rs"
  "src/components/settings/advanced/AdvancedSettings.tsx"
  "src/components/settings/general/ModelSettingsCard.tsx"
  "src-tauri/src/overlay.rs"
  "src-tauri/src/managers/model.rs"
  "src-tauri/src/cli.rs"
  "src-tauri/build.rs"
  "src-tauri/src/shortcut/handler.rs"
  "src-tauri/src/tray.rs"
  "src/components/settings/general/GeneralSettings.tsx"
  "src-tauri/src/audio_toolkit/audio/sink.rs"
  "src-tauri/src/engine_supervisor/protocol.rs"
  "src-tauri/src/engine_supervisor/worker.rs"
  "src/overlay/RecordingOverlay.css"
)

fail=0
pass=0

echo "==> fork-check: integration probes"
for entry in "${PROBES[@]}"; do
  IFS='|' read -r label file pattern <<<"$entry"
  if [[ ! -f "$file" ]]; then
    printf '  \033[31mMISS\033[0m %s\n         file not found: %s\n' "$label" "$file"
    fail=$((fail + 1))
    continue
  fi
  if grep -Eq -- "$pattern" "$file"; then
    printf '  \033[32mok\033[0m   %s\n' "$label"
    pass=$((pass + 1))
  else
    printf '  \033[31mMISS\033[0m %s\n         expected /%s/ in %s\n' "$label" "$pattern" "$file"
    fail=$((fail + 1))
  fi
done

echo "==> fork-check: fork(voice-control) markers in upstream-owned files"
for file in "${MARKER_FILES[@]}"; do
  if [[ -f "$file" ]] && grep -q "fork(voice-control)" "$file"; then
    printf '  \033[32mok\033[0m   %s\n' "$file"
    pass=$((pass + 1))
  else
    printf '  \033[31mMISS\033[0m marker missing in %s\n' "$file"
    fail=$((fail + 1))
  fi
done

echo ""
if (( fail == 0 )); then
  printf '\033[32m==> fork-check passed\033[0m (%d probes)\n' "$pass"
  exit 0
fi
printf '\033[31m==> fork-check FAILED: %d missing, %d ok\033[0m\n' "$fail" "$pass"
echo "    A missing probe usually means an upstream rebase dropped a fork hook."
echo "    Re-apply the hook, or if the feature is now obsolete, remove its probe"
echo "    here and its entry in docs/fork-patches.md in the same change."
exit 1
