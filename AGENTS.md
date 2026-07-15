# AGENTS.md

This file provides guidance to AI coding assistants working with code in this repository.

## Development Commands

**Prerequisites:**

- [Rust](https://rustup.rs/) (latest stable)
- [Bun](https://bun.sh/) package manager

**Core Development:**

```bash
# Install dependencies
bun install

# Run in development mode
bun run tauri dev
# If cmake error on macOS:
CMAKE_POLICY_VERSION_MINIMUM=3.5 bun run tauri dev

# Build for production
bun run tauri build

# Frontend only development
bun run dev        # Start Vite dev server
bun run build      # Build frontend (TypeScript + Vite)
bun run preview    # Preview built frontend
```

**Linting and Formatting (run before committing):**

```bash
bun run lint              # ESLint for frontend
bun run lint:fix          # ESLint with auto-fix
bun run format            # Prettier + cargo fmt
bun run format:check      # Check formatting without changes
bun run format:frontend   # Prettier only
bun run format:backend    # cargo fmt only
```

**Model Setup (Required for Development):**

```bash
mkdir -p src-tauri/resources/models
curl -o src-tauri/resources/models/silero_vad_v4.onnx https://blob.handy.computer/silero_vad_v4.onnx
```

For detailed platform-specific build setup, see [BUILD.md](BUILD.md).

## Architecture Overview

Handy is a cross-platform desktop speech-to-text application built with Tauri 2.x (Rust backend + React/TypeScript frontend).

### Backend Structure (src-tauri/src/)

- `lib.rs` - Main entry point, Tauri setup, manager initialization
- `managers/` - Core business logic:
  - `audio.rs` - Audio recording and device management
  - `model.rs` - Model downloading and management
  - `transcription.rs` - Speech-to-text processing pipeline
  - `history.rs` - Transcription history storage
- `audio_toolkit/` - Low-level audio processing:
  - `audio/` - Device enumeration, recording, resampling
  - `vad/` - Voice Activity Detection (Silero VAD)
- `commands/` - Tauri command handlers for frontend communication
- `cli.rs` - CLI argument definitions (clap derive)
- `shortcut.rs` - Global keyboard shortcut handling
- `settings.rs` - Application settings management
- `overlay.rs` - Recording overlay window (platform-specific)
- `signal_handle.rs` - `send_transcription_input()` reusable function
- `utils.rs` - Platform detection helpers

### Frontend Structure (src/)

- `App.tsx` - Main component with onboarding flow
- `components/` - React UI components:
  - `settings/` - Settings UI
  - `model-selector/` - Model management interface
  - `onboarding/` - First-run experience
  - `overlay/` - Recording overlay UI
  - `update-checker/` - App update notifications
  - `shared/`, `ui/`, `icons/`, `footer/` - Shared components
- `hooks/useSettings.ts` - Settings state management hook
- `stores/settingsStore.ts` - Zustand store for settings
- `bindings.ts` - Auto-generated Tauri type bindings (via tauri-specta)
- `overlay/` - Recording overlay window entry point
- `lib/types.ts` - Shared TypeScript type definitions

### Key Architecture Patterns

**Manager Pattern:** Core functionality organized into managers (Audio, Model, Transcription) initialized at startup and managed via Tauri state.

**Command-Event Architecture:** Frontend → Backend via Tauri commands; Backend → Frontend via events.

**Pipeline Processing:** Audio → VAD → Whisper/Parakeet → Text output → Clipboard/Paste

**State Flow:** Zustand → Tauri Command → Rust State → Persistence (tauri-plugin-store)

### Technology Stack

**Core Libraries:**

- `transcribe-cpp` - Local Whisper-family inference (GGML/GGUF) with GPU acceleration
- `transcribe-rs` - ONNX speech recognition (Parakeet, Moonshine, SenseVoice, etc.)
- `cpal` - Cross-platform audio I/O
- `vad-rs` - Voice Activity Detection
- `rdev` - Global keyboard shortcuts
- `rubato` - Audio resampling
- `rodio` - Audio playback for feedback sounds

### Application Flow

1. **Initialization:** App starts minimized to tray, loads settings, initializes managers
2. **Model Setup:** First-run downloads preferred Whisper model (Small/Medium/Turbo/Large)
3. **Recording:** Global shortcut triggers audio recording with VAD filtering
4. **Processing:** Audio sent to Whisper model for transcription
5. **Output:** Text pasted to active application via system clipboard

### Settings System

Settings are stored using Tauri's store plugin with reactive updates:

- Keyboard shortcuts (configurable, supports push-to-talk)
- Audio devices (microphone/output selection)
- Model preferences (Small/Medium/Turbo/Large Whisper variants)
- Audio feedback and translation options

### Single Instance Architecture

The app enforces single instance behavior — launching when already running brings the settings window to front rather than creating a new process. Remote control flags (`--toggle-transcription`, etc.) work by launching a second instance that sends args to the running instance via `tauri_plugin_single_instance`, then exits.

## Internationalization (i18n)

All user-facing strings must use i18next translations. ESLint enforces this (no hardcoded strings in JSX).

**Adding new text:**

1. Add key to `src/i18n/locales/en/translation.json`
2. Use in component: `const { t } = useTranslation(); t('key.path')`

**File structure:**

```
src/i18n/
├── index.ts           # i18n setup
├── languages.ts       # Language metadata
└── locales/
    ├── en/translation.json  # English (source)
    ├── de/, es/, fr/, ja/, ru/, zh/, ...
    └── ...
```

For translation contribution guidelines, see [CONTRIBUTING_TRANSLATIONS.md](CONTRIBUTING_TRANSLATIONS.md).

## Code Style

**Rust:**

- Run `cargo fmt` and `cargo clippy` before committing
- Handle errors explicitly (avoid unwrap in production)
- Use descriptive names, add doc comments for public APIs

**TypeScript/React:**

- Strict TypeScript, avoid `any` types
- Functional components with hooks
- Tailwind CSS for styling
- Path aliases: `@/` → `./src/`

## CLI Parameters

Handy supports command-line parameters on all platforms for integration with scripts, window managers, and autostart configurations.

**Implementation:** `cli.rs` (definitions), `main.rs` (parsing), `lib.rs` (applying), `signal_handle.rs` (shared logic)

| Flag                     | Description                                                |
| ------------------------ | ---------------------------------------------------------- |
| `--toggle-transcription` | Toggle recording on/off on a running instance              |
| `--toggle-post-process`  | Toggle recording with post-processing on/off               |
| `--cancel`               | Cancel the current operation on a running instance         |
| `--start-hidden`         | Launch without showing the main window (tray icon visible) |
| `--no-tray`              | Launch without system tray (closing window quits the app)  |
| `--debug`                | Enable debug mode with verbose (Trace) logging             |

**Key design decisions:**

- CLI flags are runtime-only overrides — they do NOT modify persisted settings
- Remote control flags work via `tauri_plugin_single_instance`: second instance sends args, then exits
- `send_transcription_input()` in `signal_handle.rs` is shared between signal handlers and CLI

## Debug Mode

Access debug features: `Cmd+Shift+D` (macOS) or `Ctrl+Shift+D` (Windows/Linux)

## Platform Notes

- **macOS**: Metal acceleration, accessibility permissions required for keyboard shortcuts
- **Windows**: Vulkan acceleration, code signing
- **Linux**: OpenBLAS + Vulkan, limited Wayland support, overlay uses GTK layer shell (disable with `HANDY_NO_GTK_LAYER_SHELL=1`)

## Troubleshooting

See the [Troubleshooting](README.md#troubleshooting) section in README.md.

## GitHub workflow for AI coding assistants

**MANDATORY. Before opening any PR, issue, or discussion in this repo: you MUST read the relevant template file and follow it strictly.** That includes sections that look "ceremonial" — checklists, AI Assistance disclosures, "Human Written Description". A generic Summary/Test-plan layout is not acceptable.

- **Opening a PR:** Read [`.github/PULL_REQUEST_TEMPLATE.md`](.github/PULL_REQUEST_TEMPLATE.md). Every section listed there is mandatory. If a section requires a human-written paragraph (e.g. "Human Written Description"), leave a clear TODO placeholder and ask the human contributor to fill it in — do not invent their voice.
- **Opening an issue:** Read [`.github/ISSUE_TEMPLATE/`](.github/ISSUE_TEMPLATE/). Blank issues are disabled; pick the right template (`bug_report.md` for bugs). Feature requests do not belong in issues — they go to [Discussions](https://github.com/cjpais/Handy/discussions) (see `.github/ISSUE_TEMPLATE/config.yml`).
- **Proposing a feature:** Handy is under a feature freeze. New features require community support gathered in [Discussions](https://github.com/cjpais/Handy/discussions) before any PR is opened — see the PR template's "Community Feedback" section.
- **Translations:** Follow [CONTRIBUTING_TRANSLATIONS.md](CONTRIBUTING_TRANSLATIONS.md).
- **Full contributor workflow:** [CONTRIBUTING.md](CONTRIBUTING.md).

**Commits:** Use conventional commit prefixes (`feat:`, `fix:`, `docs:`, `refactor:`, `chore:`). Focus the message on _why_, not _what_.

---

<!-- ============================================================== -->
<!-- FORK SECTION — everything below is fork-specific (voice-control).
     Kept as one additive block at the end of the file to minimize
     rebase conflicts with upstream. On conflict: keep upstream's
     changes above, keep this block below. -->
<!-- ============================================================== -->

# Fork Workflow (voice-control)

This repository is a long-lived personal fork of [cjpais/Handy](https://github.com/cjpais/Handy) focused on voice-control/dictation ergonomics (see [docs/REQUIREMENTS.md](docs/REQUIREMENTS.md)). Upstream is under a feature freeze, so fork-only features live here permanently. `CLAUDE.md` is a symlink to this file.

## Remotes & Branches

| Ref                         | Role                                                                                                                                                                                                    |
| --------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `upstream` → `cjpais/Handy` | Source of truth for `main`. Never push here.                                                                                                                                                            |
| `origin` → `sachssem/Handy` | Our fork on GitHub. All pushes go here.                                                                                                                                                                 |
| `main`                      | **Pure upstream mirror.** Tracks `upstream/main`. Only ever updated via fast-forward (`git fetch upstream && git merge --ff-only upstream/main`), then pushed to `origin main`. NEVER commit to `main`. |
| `voice-control`             | **Integration branch — the fork's real mainline.** All fork patches live here as clean, logically separated commits. Release builds (DMG) are built from this branch.                                   |
| `feat/*`                    | Short-lived feature branches, cut from `voice-control`. Rebase-merged back as 1–3 clean commits (no merge commits), then deleted.                                                                       |

## Upstream Update Procedure

```bash
git fetch upstream
git checkout main && git merge --ff-only upstream/main
git push origin main
git tag backup/voice-control-$(date +%Y%m%d) voice-control   # safety net
git rebase main voice-control
# resolve conflicts; fork changes are additive, so conflicts should be rare
git push --force-with-lease origin voice-control
```

Rules:

- Always create the backup tag before rebasing; delete old backup tags once the rebase is verified.
- Only `--force-with-lease` (never `--force`) and only on `voice-control`/`feat/*`, never on `main`.
- After a rebase, verify with `cargo test` and a `bun run build` before pushing.

## Fork Design Rules (keep rebases cheap)

- **Additive over invasive:** new functionality goes into new modules/files; touch upstream files only at narrow, stable insertion points (e.g. one call in the transcription output pipeline). Prefer one clearly marked block over scattered edits.
- **Fork-owned files** (safe to change freely, upstream never touches them): `docs/REQUIREMENTS.md`, this Fork section, and any module introduced by a fork feature (each feature's commit message lists its new files).
- **One feature = one reviewable commit group.** Features must remain individually droppable via `git rebase -i` if upstream ships an equivalent.
- Follow all upstream conventions above (i18n for user-facing strings, `cargo fmt`/`clippy`, ESLint, conventional commits). Fork feature commits use the normal `feat:` prefix.
- Do NOT open PRs/issues against upstream for fork-only features (upstream feature freeze); the "GitHub workflow" section above applies only when intentionally contributing upstream.

## Fork Debug Helpers

- `handy --debug-toast` (hidden CLI flag, forwarded to the running instance via
  single-instance): shows a sample learned-correction trial toast through the
  exact production display path — the repeatable test for toast regressions
  without a dictation + manual-correction round trip. Display-chain breadcrumbs
  (`toast-show`, `toast-webview: …`, `toast-hide`) log at debug level.

## Fork Build (local signed DMG)

Build release DMGs with the fork script, not `bun run tauri build` directly:

```bash
scripts/build-signed-dmg.sh
```

This is the required release-build workflow for agents and humans working on
the fork. Finished DMGs are moved to the ignored repo-root directory
`Handy artifacts/`; only the three newest matching voice-control DMGs are kept.
After a successful build, Cargo intermediates are removed automatically. A
failed build deliberately keeps them for diagnosis. For an exceptional follow-up
build where retaining the cache is useful, opt out explicitly:

```bash
HANDY_KEEP_BUILD_ARTIFACTS=1 scripts/build-signed-dmg.sh
```

Do not invoke `bun run tauri build` for a releasable local DMG and do not move
DMGs back under `src-tauri/target`; that tree is disposable. Dev and test
incremental compilation are disabled in `src-tauri/Cargo.toml` to prevent
multi-GB caches during ordinary checks and test runs. Do not re-enable them
without a measured need.

It handles two macOS-local issues that plain `tauri build` does not:

- **Stable code signing.** Ad-hoc signatures have an unstable identity, so macOS TCC forgets Microphone/Accessibility grants on every rebuild (app shows the permission as granted in System Settings while Handy still reports it as pending). The script signs with a stable self-signed code-signing certificate, making the app's Designated Requirement constant so permissions persist across rebuilds. Default identity: `Voice Control / Handy Dev`; override with `HANDY_SIGN_IDENTITY="<name>"` (list via `security find-identity -v`). The cert is a free, self-signed **Code Signing** certificate created in Keychain Access → Certificate Assistant (no paid Apple Developer ID). `codesign` accepts it even though it shows as `CSSMERR_TP_NOT_TRUSTED` / not a "valid identity"; no keychain trust change is required.
- **Reliable DMG packaging.** Tauri's `bundle_dmg.sh` (AppleScript Finder layout) fails intermittently on this machine, so the script builds only the `.app` (`--bundles app`) and packages the DMG via `hdiutil`.
- **Bounded disk use.** The durable output lives in `Handy artifacts/`, retention is capped at three DMGs, and successful builds clean Cargo intermediates by default.

Notes:

- The build is **not notarized** (no paid Apple Developer ID). First launch of each new build needs one right-click → Open (Gatekeeper). This is separate from the permission persistence above and cannot be removed without notarization.
- If permissions get stuck after switching signing identity once (the old grants were tied to the previous identity), reset them once: `tccutil reset Microphone com.pais.handy` and `tccutil reset Accessibility com.pais.handy`, then re-grant. Subsequent same-cert rebuilds keep the grants.
