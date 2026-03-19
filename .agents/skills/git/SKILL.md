---
name: git
description: Use when committing, branching, rebasing, searching history, or opening PRs. Kley operates via branch-and-submit-PR with its own Git creds. Do not use for code editing, testing, or build tasks.
---

## Repository layout

- **origin** (SSH): `git@github.com:zakstak/kley.git` — push target (requires saga-agent SSH key)
- **upstream** (HTTPS): `https://github.com/zakstak/kley.git` — read-only fallback for fetch/pull
- Default branch: `main`
- Git identity: `saga <saga@zakstak.dev>`
- GitHub CLI user: `saga-agent`

Kley pushes feature branches to `origin` and opens PRs against `main` on `zakstak/kley`. Never push directly to `main`.

## Commit procedure

1. Gather context: `git status -s`, `git diff --stat`, `git log -20 --oneline`.
2. Detect the repo's commit style from recent history. Default to `type(scope): subject`.
3. Plan atomic commits. Split by module, concern, and independent revertability. Pair implementation with its tests.
4. Branch from fresh main: `git switch -c <type>/<name>` from an up-to-date `main`.
5. Stage and commit each group separately. Verify staging with `git diff --cached --stat`.
6. Push to origin and open PR: `git push -u origin HEAD`, then `gh pr create --repo zakstak/kley --base main --head <branch>`.

## Commit split rules

- Different directories or modules → separate commits.
- Different concerns (logic / test / config / docs) → separate commits.
- 3+ files → at least 2 commits. 5+ files → at least 3 commits.
- Only combine when splitting would break compilation AND both files serve the same atomic unit.

## Self-improvement workflow

The self-improvement harness (`self-improve.sh`) runs a RALF-style loop:
- Each cycle is a fresh `kley chat` invocation with a clean context window.
- Branches follow the `improve/<short-slug>` naming convention.
- PRs are opened non-interactively with structured bodies (Problem/Before/After/Tests/Validation).
- The loop auto-stops on `blocked` or after 3 consecutive `no-safe-change` results.

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

- Push directly to `main`
- Make one giant commit from multiple unrelated changes
- Rebase without checking if force-push is safe
- Commit without running the project's pre-commit checks first
- Force-push or rewrite history on `main`
- Merge `main` into a feature branch (rebase instead)

## Done checklist

- Branch created from up-to-date `main`
- Commits are atomic and style-matched
- PR opened against `zakstak/kley` `main`
- Pre-commit checks passed before every commit
