---
name: git
description: Use when committing, branching, rebasing, searching history, or opening PRs. Kley operates via fork-and-submit-PR with its own Git creds. Do not use for code editing, testing, or build tasks.
---

Kley owns a fork (`origin`) and submits PRs against `upstream`. Never push to upstream directly.

## Commit procedure

1. Gather context: `git status -s`, `git diff --stat`, `git log -20 --oneline`.
2. Detect the repo's commit style from recent history. Default to `type(scope): subject`.
3. Plan atomic commits. Split by module, concern, and independent revertability. Pair implementation with its tests.
4. Branch from fresh upstream: `git checkout -b <type>/<name> upstream/main`.
5. Stage and commit each group separately. Verify staging with `git diff --cached --stat`.
6. Push to fork and open PR: `git push origin <branch>`, then `gh pr create`.

## Commit split rules

- Different directories or modules → separate commits.
- Different concerns (logic / test / config / docs) → separate commits.
- 3+ files → at least 2 commits. 5+ files → at least 3 commits.
- Only combine when splitting would break compilation AND both files serve the same atomic unit.

## Rebase procedure

1. Assess safety: is the branch pushed? Will force-push break collaborators?
2. `git rebase -i <base>`. Use `fixup` / `squash` / `reword` / `drop`.
3. For `fixup!` commits: `git rebase -i --autosquash <base>`.
4. On conflict: resolve, `git add`, `git rebase --continue`. If stuck: `git rebase --abort`.
5. Verify: `git log --oneline -10`, diff against pre-rebase to confirm no content lost.

## History search

- When was a string added/removed: `git log -S "string" --oneline`
- Pattern changes: `git log -G "pattern" --oneline`
- Who last touched lines: `git blame -L <range> <file>`
- Binary search for regression: `git bisect start`, mark good/bad, test each step.

## Never

- push directly to upstream
- make one giant commit from multiple unrelated changes
- rebase without checking if force-push is safe
- commit without running the project's pre-commit checks first

## Done

- branch created from fresh `upstream/main`
- commits are atomic and style-matched
- PR opened against upstream
- pre-commit checks passed before every commit
