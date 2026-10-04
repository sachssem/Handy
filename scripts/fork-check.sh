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
#   - after every `scripts/sync-upstream.sh` rebase, before pushing.
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
  "correction-learning: shortcut commands|src-tauri/src/shortcut/mod.rs|correction_learning::"
  "correction-learning: settings fields|src-tauri/src/settings.rs|learned_corrections"
  "correction-learning: settings UI mounted|src/components/settings/advanced/AdvancedSettings.tsx|LearnedCorrections"

  # -- language-allowlist guard (+ fallback model) --
  "language-allowlist: transcription guard|src-tauri/src/managers/transcription.rs|language_allowlist"
  "language-allowlist: shortcut command|src-tauri/src/shortcut/mod.rs|change_language_allowlist_setting"
  "language-allowlist: settings field|src-tauri/src/settings.rs|language_allowlist"

  # -- recording-limit auto-stop --
  "recording-limit: settings field|src-tauri/src/settings.rs|auto_stop_recording_on_limit"
  "recording-limit: settings command|src-tauri/src/shortcut/mod.rs|auto_stop_recording_on_limit"
  "recording-limit: enforcement scheduler|src-tauri/src/transcription_coordinator.rs|schedule_recording_limit"
  "recording-limit: settings UI mounted|src/components/settings/advanced/AdvancedSettings.tsx|RecordingLimitAutoStop"

  # -- overlay: instant, compact, theme-inverted pill --
  "overlay: instant show (no awaits before paint)|src/overlay/RecordingOverlay.tsx|flushSync\\(\\(\\) =>"
  "overlay: compact capsule|src/overlay/RecordingOverlay.css|--ov-capsule-w"
  "overlay: window sized for capsule|src-tauri/src/overlay.rs|OVERLAY_WIDTH: f64 = 164"

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
  "src-tauri/src/overlay.rs"
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
