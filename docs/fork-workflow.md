# Fork workflow (voice-control)

Procedures for the personal fork. The rules agents must always follow stay in the "Fork Workflow" section of [`AGENTS.md`](../AGENTS.md); this file holds the steps and background behind them.

## Upstream Update Procedure

**Use the script — do not hand-run the steps:**

```bash
scripts/sync-upstream.sh            # fetch, ff-only main, backup tag, squash-merge, fork-check, verify
scripts/sync-upstream.sh --continue # after resolving conflicts on the sync branch
scripts/sync-upstream.sh --push     # ...and push main + voice-control at the end
```

The script performs, in order: fetch upstream, fast-forward `main` to
`upstream/main` (aborts if `main` ever diverged), cut a `backup/voice-control-*`
safety tag, squash-merge `voice-control` onto `main` on a fresh
`sync/upstream-<sha>` branch, run `scripts/fork-check.sh`, verify with
`cargo test` + `bun run build`, move `voice-control` to the verified result,
prune old backup tags (keeps newest 3), and print the push commands. It
refuses to start on a dirty tree.

Why a squash and not a rebase: the fork grafts into a handful of upstream
files, and a rebase replays every fork commit, so one upstream change to
`managers/transcription.rs` conflicts once per commit that ever touched it
(the v0.10.1 sync would have hit 85 commits). The squash resolves each
conflict once. The squash commit's body lists the subjects of the fork commits
it absorbed, and the backup tag keeps their full history.

On a conflict it stops with instructions; `voice-control` is untouched and
you are on the sync branch. Resolve keeping the fork hook, but first run the
conflicted feature's **Upstream check** in
[`docs/fork-patches.md`](fork-patches.md) — if upstream now ships an
equivalent, **drop** the feature (its files, its probe, its doc section)
instead of adapting it. Fork modules that merged without a textual conflict can
still fail to compile against new upstream APIs. Then `git add` the files and
run `scripts/sync-upstream.sh --continue`. To give up, commit the work in
progress on the sync branch and check out `voice-control`; there is no rebase
state to abort.

Rules:

- Pushing is opt-in (`--push`): it force-pushes `voice-control`, so it stays a
  deliberate step. Only `--force-with-lease` (never `--force`), only on
  `voice-control`/`feat/*`, never on `main`.
- The backup tag is created automatically; the script prunes to the newest 3.

## Fork Debug Helpers

- `handy --debug-toast` (hidden CLI flag, forwarded to the running instance via
  single-instance): shows a sample learned-correction suggestion toast through
  the exact production display path — the repeatable test for toast
  regressions without a dictation + manual-correction round trip. It also
  arms the toast shortcuts (default ⌃↩, plus ⌃⎋ to dismiss; ⌃⌫ Undo only
  arms for learned pairs) like a real toast; its ids match no stored pair, so
  Accept is a harmless no-op. It must not take
  keyboard focus: typing in another app while it shows has to keep working.
  Display-chain breadcrumbs (`toast-show`, `toast-webview: …`, `toast-hide`,
  `learned-toast shortcut …`) log at debug level.
- Overlay latency breadcrumbs (debug level): the press line
  `TranscribeAction::start … epoch_ms=`, the backend show line
  `overlay '<state>': … epoch_ms=` and the webview lines
  `toast-webview: overlay: show '<state>' handler|first-frame epoch_ms=…` line
  up the press with the overlay's first frame; the same data lands in the
  dictation journal as `overlay` events. Journal schema and queries:
  [docs/journal.md](journal.md) — analyse the journal before asking the
  user for samples.

## Fork Build (local signed DMG)

Build release DMGs with the fork script, not `bun run tauri build` directly:

```bash
scripts/build-signed-dmg.sh
```

This is the required release-build workflow for agents and humans working on
the fork. Finished DMGs are moved to the ignored directory `Handy artifacts/`
in the **main checkout** (`~/Code/Handy/Handy artifacts/`), also when the build
runs in a linked worktree — the script resolves it via `git --git-common-dir`;
never leave a DMG in a worktree's own folder. Only the three newest matching
voice-control DMGs are kept.
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
- The fork **never self-updates**: `update_checks_forced_disabled()` is hard-wired to `true` (see [docs/fork-patches.md](fork-patches.md#fork-never-self-updates)), so the in-app updater can never replace the fork with an official upstream release. Updates come only from rebuilding this branch.
- **Homebrew:** if the official `handy` cask is installed, `brew upgrade --greedy` / `brew reinstall` will overwrite the fork app with the upstream release. Run `brew uninstall --cask handy` before installing the fork DMG (without `--zap` this keeps the app data: settings, history, models).
