#!/usr/bin/env bash
# Smoke-test that git + GitHub access is properly configured.
# Run this before self-improve.sh to catch permission issues early.
# Works both on the host and inside Docker.

PASS=0
FAIL=0

check() {
  local label="$1"
  shift
  if "$@" >/dev/null 2>&1; then
    echo "  ✓ $label"
    PASS=$((PASS+1))
  else
    echo "  ✗ $label"
    FAIL=$((FAIL+1))
  fi
}

echo "── Running from: $(pwd) ──"
echo "   Git user:  $(git config user.name 2>/dev/null || echo '(not set)')"
echo "   Git email: $(git config user.email 2>/dev/null || echo '(not set)')"
echo "   GitHub:    $(gh api user --jq .login 2>/dev/null || echo '(not authenticated)')"
echo ""

echo "── Git access checks ──"
check "git is installed"          git --version
check "inside a git repo"        git rev-parse --is-inside-work-tree
check "origin remote exists"     git remote get-url origin
check "can fetch from origin"    git ls-remote origin HEAD

echo ""
echo "── GitHub CLI checks ──"
check "gh is installed"           gh --version
check "gh is authenticated"       gh auth status
check "can list PRs on origin"    gh pr list --limit 1

echo ""
echo "── Rust toolchain ──"
check "cargo is installed"        cargo --version
check "cargo fmt available"       cargo fmt --version
check "cargo clippy available"    cargo clippy --version
check "kley binary works"        kley --help

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━"
echo "  Passed: $PASS  Failed: $FAIL"
echo "━━━━━━━━━━━━━━━━━━━━━━"

if [ "$FAIL" -gt 0 ]; then
  echo ""
  echo "⚠ Fix the failing checks above before running self-improve.sh"
  exit 1
else
  echo ""
  echo "✓ All checks passed — ready to self-improve!"
fi
