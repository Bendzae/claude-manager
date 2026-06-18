---
name: stacked-pr
description: Work on a Claude Manager task whose changes ship as a STACK of dependent PRs (one PR per commit, via `git spr`). Use when the task has stacked-PR mode enabled, or the user wants to split a task's work into a chain of small, dependent, independently-reviewable PRs.
user-invocable: true
---

In Claude Manager a task can be put in **stacked-PR mode** (context menu → "Enable stacked PRs", default key `s`). A stacked task publishes **one PR per commit** on the task branch via [`git spr`](https://github.com/ejoffe/spr) instead of a single PR. The bottom commit (closest to trunk) merges first; the top commit (HEAD) merges last.

```
task branch:
 ● commit "Add user model"   → PR #1  (base: main)   ← bottom, merges first
 ● commit "Add user API"     → PR #2  (base: PR #1)
 ● commit "Add user UI"      → PR #3  (base: PR #2)   ← top, merges last
```

Your job in a session (a worktree) is to **shape the commits** so each one is a clean, PR-sized, dependency-ordered unit, then **merge them into the task branch** and **publish** the stack. Publishing always runs on the **task branch** — drive it with the `claude-manager` CLI (below) or let the user use the TUI. Never run `git spr` directly in the worktree (that would publish the worktree branch as a separate, diverging stack).

## First: enable stacked-PR mode for the task

When this skill is invoked, or whenever the user asks to make/split this task's work into a stack of PRs, **turn on stacked-PR mode for the task automatically** so the TUI publishes a stack (not a single PR). Don't wait to be told — enable it as soon as it's clear stacking is wanted.

```bash
# Project path = the MAIN repo (not this worktree). Get it from the first
# `git worktree list` entry. Branch = the "Task branch:" from the session reminder.
MAIN_REPO=$(git worktree list --porcelain | sed -n 's/^worktree //p' | head -1)
claude-manager set-stacked "$MAIN_REPO" "<task-branch>" on
```

This flips the task's `stacked` flag in Claude Manager's config; the running TUI picks it up on its next idle refresh (the task row then shows the `⑆ stacked` badge). If `claude-manager` isn't on `PATH`, tell the user to enable stacked mode from the task's context menu (key `s`) and continue. To turn it back off: `claude-manager set-stacked "$MAIN_REPO" "<task-branch>" off`.

## The golden rule: one commit = one PR

- **Each commit is a discrete, reviewable unit.** The commit **subject is the PR title**, the **body is the PR description** — write real messages, not `wip`.
- **Order by dependency, bottom → top.** Foundational changes (models, schema, shared utils) go in earlier (lower) commits; dependents (API, UI, consumers) go in later (higher) commits. If B depends on A, A must be the same or an earlier commit.
- **Never strip the `commit-id:` trailer** that spr adds to each commit body on first publish — it maps a commit to its PR across amends/rebases. Native `git commit --amend` and `git rebase` preserve it; a full-body rewrite or squash that drops it orphans the PR.

## Workflow

### 1. Make stack-shaped commits in the worktree

Work normally, but commit deliberately — one cohesive change per commit, in dependency order:

```
git add internal/models/user.go
git commit -m "Add user model" -m "Introduces the User type and its persistence layer."

git add internal/api/users.go
git commit -m "Add user API" -m "CRUD endpoints backed by the user model."
```

### 2. Fold a fix into an EARLIER commit (not the tip)

This is the common case: you changed the backend, but the backend already has its own commit/PR lower in the stack. Put the fix **in that commit**, not as a new commit on top (which would become its own PR, or land in the wrong PR).

```
# make the edit, then commit it as a fixup targeting the owning commit
git add <changed files>
git commit --fixup=<target>        # <target> = the commit's SHA, or :/Add user model (subject match)

# autosquash folds the fixup into its target, non-interactively
GIT_SEQUENCE_EDITOR=: git rebase -i --autosquash <root>
#   <root> = origin/main (or the task's base branch)
```

Find the target SHA with `git log --oneline <base>..HEAD`. Autosquash moves the `fixup!` commit next to its target and squashes it — even past intervening commits. A conflict only arises if the fix touches lines a later commit also changed; resolve it like any rebase conflict (`git add` → `git rebase --continue`).

### 3. Sync the worktree into the task branch

Use the **`commit-push-task`** skill. For stacked tasks the task branch history is rewritten on each publish (spr rebases onto trunk + adds trailers), so a pre-publish worktree may no longer fast-forward. `commit-push-task` handles this by falling back to **rebase-then-fast-forward** (rebase the worktree's new commits onto the task tip, then ff). Resolve any rebase conflict in the worktree.

### 4. Publish / refresh / sync the stack

Publishing always operates on the **task branch** (the stack's source of truth), so **merge your worktree into the task branch first** (step 3), then publish. The stack is one PR per commit on the task branch — not on your worktree branch.

You can drive this yourself with the `claude-manager` CLI (it runs the same `git spr` operations the TUI does, on the task branch, and updates the cache the UI reads):

```bash
MAIN_REPO=$(git worktree list --porcelain | sed -n 's/^worktree //p' | head -1)

# publish or refresh the stack (git spr update): create one PR per commit,
# or update existing PRs in place after edits/amends
claude-manager stack-publish "$MAIN_REPO" "<task-branch>"

# after some PRs merge on GitHub or trunk moves: reconcile (git spr sync) —
# rebases the remaining stack onto trunk and updates PRs
claude-manager stack-sync "$MAIN_REPO" "<task-branch>"
```

Or the user can do it from the TUI: **Open PR** (`o`) to publish, **Update branch** (`u`) to refresh.

**These are outward-facing** — they create/update real GitHub PRs. Only publish when the user has asked to (or clearly authorized it); don't publish a stack unprompted. Do **not** merge — `git spr merge` is a human step (see Agent rules).

After a `stack-sync` rewrites the task branch, your worktree may no longer fast-forward — re-sync it with the rebase-then-ff path in step 3 before continuing.

> Run `stack-publish`/`stack-sync` from the worktree (the CLI figures out the main repo). Do **not** run `git spr` directly in the worktree — that would publish the *worktree* branch's commits as a separate stack and diverge from the task branch.

## Agent rules (keep `git spr` non-interactive)

These apply if you ever run `spr` directly (normally the TUI does it):

1. **Never run `git spr amend` or `git spr edit`** — they open interactive pickers. Amend commits with native `git commit --amend` / `git rebase --autosquash` instead.
2. **Rebase onto trunk with `git rebase`, never `git merge`** — a merge commit in the range breaks the one-commit-one-PR model.
3. **Prefix a commit subject with `WIP`** to skip PR creation for an in-progress top commit; remove the prefix and republish when ready.
4. **Don't merge.** `git spr merge` is a human step (GitHub forbids approving your own PR). Tell the user to merge via `git spr merge`, **never the GitHub UI** (the UI breaks the chained bases).

## Prerequisites

- `git spr` installed (`brew install ejoffe/tap/spr`) and on PATH — only needed where publishing runs (the TUI host).
- GitHub auth via `gh auth login` or `GITHUB_TOKEN` (spr reuses them).
- Claude Manager silences spr's interactive star prompt automatically (`stargazer: true` in `~/.spr.yml`).

## Gotchas

- **A fix landed in the wrong PR** → you committed it on top instead of `--fixup`-ing the owning commit. Move it: `git rebase -i` to reorder/squash, then republish.
- **A duplicate PR appeared** → the `commit-id:` trailer was dropped from a commit body. Restore the original `commit-id:` line and republish.
- **Worktree won't fast-forward after a publish** → expected; the task branch was rewritten. `commit-push-task` rebases the worktree onto the task tip first. Resolve conflicts in the worktree.
