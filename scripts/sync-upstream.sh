#!/usr/bin/env bash
#
# fork(voice-control): one-command upstream sync for the personal fork.
#
# Codifies the "Upstream Update Procedure" from AGENTS.md so the safety steps
# (backup tag, ff-only mirror, rebase, integrity gate) can never be skipped or
# mis-typed. It performs everything up to and including the rebase + verification,
# then STOPS and prints the exact push commands. Pushing is a force-with-lease on
# a shared branch, so it stays a deliberate human step (pass --push to include it).
#
# Usage:
#   scripts/sync-upstream.sh            # fetch, mirror main, backup, rebase, verify
#   scripts/sync-upstream.sh --push     # ...and push main + voice-control at the end
#   scripts/sync-upstream.sh --no-verify# skip the cargo test / bun build gate (fast)
#   scripts/sync-upstream.sh --no-fetch # skip `git fetch` (use already-fetched refs;
#                                       # for sandboxed/offline runs)
#
# Preconditions (the script checks them): clean working tree, `upstream` and
# `origin` remotes present, on a real Handy checkout. It never touches `main`
# except via fast-forward and never force-pushes anything but `voice-control`.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

FORK_BRANCH="voice-control"
MIRROR_BRANCH="main"
UPSTREAM_REMOTE="upstream"
ORIGIN_REMOTE="origin"
DO_PUSH=0
DO_VERIFY=1
DO_FETCH=1

for arg in "$@"; do
  case "$arg" in
    --push) DO_PUSH=1 ;;
    --no-verify) DO_VERIFY=0 ;;
    --no-fetch) DO_FETCH=0 ;;
    -h|--help) grep '^#' "$0" | grep -v '^#!' | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

die() { echo "ERROR: $*" >&2; exit 1; }
step() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }

# --- Preflight ---------------------------------------------------------------
git remote get-url "$UPSTREAM_REMOTE" >/dev/null 2>&1 || die "no '$UPSTREAM_REMOTE' remote (expected cjpais/Handy)"
git remote get-url "$ORIGIN_REMOTE" >/dev/null 2>&1 || die "no '$ORIGIN_REMOTE' remote (expected sachssem/Handy)"
git show-ref --verify --quiet "refs/heads/$FORK_BRANCH" || die "branch '$FORK_BRANCH' not found"
[[ -z "$(git status --porcelain)" ]] || die "working tree is dirty — commit or stash before syncing"

START_BRANCH="$(git rev-parse --abbrev-ref HEAD)"
FORK_SHA_BEFORE="$(git rev-parse "$FORK_BRANCH")"

if (( DO_FETCH )); then
  step "Fetching $UPSTREAM_REMOTE"
  git fetch "$UPSTREAM_REMOTE" --tags --prune
else
  step "Skipping fetch (--no-fetch); using already-fetched $UPSTREAM_REMOTE refs"
fi

# Nothing to do? Bail early so we don't cut a pointless backup tag.
if git merge-base --is-ancestor "$UPSTREAM_REMOTE/$MIRROR_BRANCH" "$FORK_BRANCH"; then
  echo "    $FORK_BRANCH already contains all of $UPSTREAM_REMOTE/$MIRROR_BRANCH — nothing to sync."
  exit 0
fi

step "Fast-forwarding $MIRROR_BRANCH to $UPSTREAM_REMOTE/$MIRROR_BRANCH"
git checkout "$MIRROR_BRANCH"
git merge --ff-only "$UPSTREAM_REMOTE/$MIRROR_BRANCH" \
  || die "$MIRROR_BRANCH is not fast-forwardable to upstream — it has diverged; investigate manually"

# --- Backup tag (the step that has historically been forgotten) --------------
# Use a git-provided timestamp (committer date of the fork tip) so the script
# stays deterministic and needs no wall-clock coupling. Disambiguate with a short
# sha suffix in case two syncs land on the same commit date.
TAG_DATE="$(git show -s --format=%cd --date=format:%Y%m%d "$FORK_BRANCH")"
BACKUP_TAG="backup/${FORK_BRANCH}-${TAG_DATE}-$(git rev-parse --short "$FORK_BRANCH")"
step "Tagging pre-rebase safety net: $BACKUP_TAG"
if git rev-parse -q --verify "refs/tags/$BACKUP_TAG" >/dev/null; then
  echo "    tag already exists — reusing it"
else
  git tag "$BACKUP_TAG" "$FORK_BRANCH"
fi

# --- Rebase ------------------------------------------------------------------
step "Rebasing $FORK_BRANCH onto $MIRROR_BRANCH"
git checkout "$FORK_BRANCH"
if ! git rebase "$MIRROR_BRANCH"; then
  cat >&2 <<EOF

Rebase stopped on a conflict. This is expected when upstream changed a file a
fork feature grafts into (see docs/fork-patches.md for each feature's hook).

  1. Resolve the conflict, keeping the fork hook. For each conflicted feature,
     run its "Upstream check" in docs/fork-patches.md FIRST — upstream may have
     shipped an equivalent, in which case drop the feature instead of adapting.
  2. git add <files> && git rebase --continue   (repeat until done)
  3. Re-run: scripts/fork-check.sh
  4. Then finish manually: verify, then push with --force-with-lease.

Backup of the pre-rebase tip: $BACKUP_TAG
Abort and restore with:  git rebase --abort
EOF
  exit 1
fi

# --- Integrity gate ----------------------------------------------------------
step "Running fork-check (integration probes)"
"$REPO_ROOT/scripts/fork-check.sh" || die "fork-check failed after rebase — a hook was dropped; fix before pushing"

if (( DO_VERIFY )); then
  step "Verifying: cargo test"
  ( cd src-tauri && cargo test --quiet ) || die "cargo test failed after rebase"
  step "Verifying: bun run build"
  bun run build || die "bun run build failed after rebase"
else
  echo "    (--no-verify: skipped cargo test / bun build)"
fi

# --- Prune old backup tags (keep newest 3) -----------------------------------
step "Pruning old backup tags (keeping newest 3)"
mapfile -t OLD_TAGS < <(git tag --list "backup/${FORK_BRANCH}-*" --sort=-creatordate | tail -n +4)
if (( ${#OLD_TAGS[@]} )); then
  for t in "${OLD_TAGS[@]}"; do
    git tag -d "$t" >/dev/null
    echo "    deleted $t"
  done
else
  echo "    none to prune"
fi

FORK_SHA_AFTER="$(git rev-parse "$FORK_BRANCH")"

# --- Push (opt-in) or print the commands -------------------------------------
if (( DO_PUSH )); then
  step "Pushing $MIRROR_BRANCH and $FORK_BRANCH"
  git push "$ORIGIN_REMOTE" "$MIRROR_BRANCH"
  git push --force-with-lease "$ORIGIN_REMOTE" "$FORK_BRANCH"
else
  step "Sync complete — review, then push manually"
  cat <<EOF
    git push $ORIGIN_REMOTE $MIRROR_BRANCH
    git push --force-with-lease $ORIGIN_REMOTE $FORK_BRANCH
EOF
fi

echo ""
echo "    $FORK_BRANCH: ${FORK_SHA_BEFORE:0:9} -> ${FORK_SHA_AFTER:0:9}"
echo "    backup tag:   $BACKUP_TAG  (delete once you trust the rebase)"
[[ "$START_BRANCH" == "$FORK_BRANCH" ]] || echo "    note: you started on '$START_BRANCH'; now on '$FORK_BRANCH'."
