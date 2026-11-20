#!/usr/bin/env bash
#
# fork(voice-control): one-command upstream sync for the personal fork.
#
# Codifies the "Upstream Update Procedure" from AGENTS.md so the safety steps
# (backup tag, ff-only mirror, integrity gate) can never be skipped or mis-typed.
#
# The fork is carried as ONE squashed commit on top of upstream. The script
# squash-merges voice-control onto the new upstream on a fresh sync branch, so
# every conflict is resolved exactly once (a rebase replays each fork commit
# and hits the same upstream file again and again) and there is never a
# half-finished rebase to abort. voice-control only moves once the result
# passed the gate.
#
# Usage:
#   scripts/sync-upstream.sh            # fetch, mirror main, backup, squash-merge, verify
#   scripts/sync-upstream.sh --continue # after resolving conflicts on the sync branch
#   scripts/sync-upstream.sh --push     # ...and push main + voice-control at the end
#   scripts/sync-upstream.sh --no-verify# skip the cargo test / bun build gate (fast)
#   scripts/sync-upstream.sh --no-fetch # skip `git fetch` (use already-fetched refs;
#                                       # for sandboxed/offline runs)
#
# Preconditions (the script checks them): clean working tree, `upstream` and
# `origin` remotes present, voice-control checked out here or nowhere. It never
# touches `main` except via fast-forward and never force-pushes anything but
# `voice-control`.
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
DO_CONTINUE=0

for arg in "$@"; do
  case "$arg" in
    --push) DO_PUSH=1 ;;
    --no-verify) DO_VERIFY=0 ;;
    --no-fetch) DO_FETCH=0 ;;
    --continue) DO_CONTINUE=1 ;;
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

START_BRANCH="$(git rev-parse --abbrev-ref HEAD)"
FORK_SHA_BEFORE="$(git rev-parse "$FORK_BRANCH")"
# Git-provided date of the fork tip plus its short sha: deterministic, and
# --continue derives the same name again.
BACKUP_TAG="backup/${FORK_BRANCH}-$(git show -s --format=%cd --date=format:%Y%m%d "$FORK_BRANCH")-$(git rev-parse --short "$FORK_BRANCH")"

# Commit the staged squash. The body keeps the subjects of the fork commits
# made since the previous sync, which the squash would otherwise lose.
commit_squash() {
  {
    echo "feat: carry the voice-control fork onto upstream $(git describe --tags --always "$MIRROR_BRANCH")"
    echo
    echo "Squash of $FORK_BRANCH (${FORK_SHA_BEFORE:0:9}), kept as $BACKUP_TAG."
    echo "docs/fork-patches.md lists every feature and its hook points."
    echo
    echo "Fork commits since the previous sync:"
    git log --reverse --format='- %s' "$(git merge-base "$MIRROR_BRANCH" "$FORK_BRANCH")..$FORK_BRANCH"
  } | git commit --quiet -F -
}

if (( DO_CONTINUE )); then
  [[ "$START_BRANCH" == sync/upstream-* ]] \
    || die "--continue runs on the sync branch (sync/upstream-*), not on '$START_BRANCH'"
  SYNC_BRANCH="$START_BRANCH"
  UNMERGED="$(git diff --name-only --diff-filter=U | tr '\n' ' ')"
  [[ -z "$UNMERGED" ]] || die "unresolved conflicts remain: $UNMERGED"
  [[ -z "$(git diff --name-only)" && -z "$(git ls-files --others --exclude-standard)" ]] \
    || die "unstaged or untracked files: 'git add' what belongs to the resolution first"
  if ! git diff --cached --quiet; then
    step "Committing the resolved squash"
    commit_squash
  fi
else
  [[ -z "$(git status --porcelain)" ]] || die "working tree is dirty — commit or stash before syncing"

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

  # A ref-to-ref fetch only fast-forwards and needs no checkout of main.
  step "Fast-forwarding $MIRROR_BRANCH to $UPSTREAM_REMOTE/$MIRROR_BRANCH"
  git fetch . "refs/remotes/$UPSTREAM_REMOTE/$MIRROR_BRANCH:refs/heads/$MIRROR_BRANCH" \
    || die "$MIRROR_BRANCH is not fast-forwardable to upstream (diverged, or checked out in another worktree)"

  step "Tagging pre-sync safety net: $BACKUP_TAG"
  if git rev-parse -q --verify "refs/tags/$BACKUP_TAG" >/dev/null; then
    echo "    tag already exists — reusing it"
  else
    git tag "$BACKUP_TAG" "$FORK_BRANCH"
  fi

  SYNC_BRANCH="sync/upstream-$(git rev-parse --short "$MIRROR_BRANCH")"
  step "Squash-merging $FORK_BRANCH onto $MIRROR_BRANCH on $SYNC_BRANCH"
  git checkout --quiet -B "$SYNC_BRANCH" "$MIRROR_BRANCH"
  if ! git merge --squash "$FORK_BRANCH" >/dev/null; then
    BASE_SHA="$(git merge-base "$MIRROR_BRANCH" "$FORK_BRANCH")"
    cat >&2 <<EOF

The squash-merge left conflicts. This is expected when upstream changed a file a
fork feature grafts into (see docs/fork-patches.md for each feature's hook).
$FORK_BRANCH is untouched; you are on $SYNC_BRANCH.

  1. git diff --name-only --diff-filter=U     (the conflicted files)
  2. Resolve them, keeping the fork hook. For each conflicted feature, run its
     "Upstream check" in docs/fork-patches.md FIRST: upstream may have shipped
     an equivalent, in which case drop the feature instead of adapting it.
     Fork modules that merged cleanly can still fail to compile against new
     upstream APIs, so run cargo check and the frontend typecheck too.
  3. git add <files>
  4. scripts/sync-upstream.sh --continue

The three versions of a file:
  base      git show ${BASE_SHA:0:9}:<path>
  upstream  git show $MIRROR_BRANCH:<path>
  fork      git show $FORK_BRANCH:<path>
Give up: commit the work in progress on $SYNC_BRANCH, then check out $FORK_BRANCH.
EOF
    exit 1
  fi
  commit_squash
fi

# --- Integrity gate ----------------------------------------------------------
FIX_HINT="fix it, commit on $SYNC_BRANCH, then run --continue"
step "Running fork-check (integration probes)"
"$REPO_ROOT/scripts/fork-check.sh" || die "fork-check failed after the sync, a hook was dropped: $FIX_HINT"

if (( DO_VERIFY )); then
  step "Verifying: cargo test"
  ( cd src-tauri && cargo test --quiet ) || die "cargo test failed after the sync: $FIX_HINT"
  step "Verifying: bun run build"
  bun run build || die "bun run build failed after the sync: $FIX_HINT"
else
  echo "    (--no-verify: skipped cargo test / bun build)"
fi

# --- Move voice-control to the verified result -------------------------------
step "Moving $FORK_BRANCH to the verified result"
git checkout --quiet -B "$FORK_BRANCH" "$SYNC_BRANCH" \
  || die "cannot move $FORK_BRANCH (checked out in another worktree?); the result stays on $SYNC_BRANCH"
git branch --quiet -d "$SYNC_BRANCH"

# --- Prune old backup tags (keep newest 3) -----------------------------------
# No `mapfile`: macOS ships bash 3.2.
step "Pruning old backup tags (keeping newest 3)"
PRUNED=0
while IFS= read -r old_tag; do
  [[ -n "$old_tag" ]] || continue
  git tag -d "$old_tag" >/dev/null
  echo "    deleted $old_tag"
  PRUNED=1
done < <(git tag --list "backup/${FORK_BRANCH}-*" --sort=-creatordate | tail -n +4)
(( PRUNED )) || echo "    none to prune"

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
echo "    backup tag:   $BACKUP_TAG  (delete once you trust the sync)"
[[ "$START_BRANCH" == "$FORK_BRANCH" ]] || echo "    note: you started on '$START_BRANCH'; now on '$FORK_BRANCH'."
