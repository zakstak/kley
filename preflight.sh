#!/usr/bin/env bash
# Smoke-test that git + GitHub access is properly configured.
# Run this before self-improve.sh to catch permission issues early.
# Works both on the host and inside Docker.
#
# Core checks (git, gh, Rust) always fail hard.
# Docker-only toolchain checks warn on the host but fail inside Docker.

PASS=0
FAIL=0
WARN=0

SELECTED_REMOTE=""

# Detect if running inside Docker
if [ -f /.dockerenv ]; then
  IN_DOCKER=true
else
  IN_DOCKER=false
fi

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

# Soft check: warns on host, fails inside Docker
optional() {
  local label="$1"
  shift
  if "$@" >/dev/null 2>&1; then
    echo "  ✓ $label"
    PASS=$((PASS+1))
  elif [ "$IN_DOCKER" = true ]; then
    echo "  ✗ $label"
    FAIL=$((FAIL+1))
  else
    echo "  ⚠ $label (optional on host)"
    WARN=$((WARN+1))
  fi
}

select_remote() {
  if git ls-remote origin HEAD >/dev/null 2>&1; then
    SELECTED_REMOTE="origin"
    return 0
  fi

  if git ls-remote upstream HEAD >/dev/null 2>&1; then
    SELECTED_REMOTE="upstream"
    return 0
  fi

  SELECTED_REMOTE=""
  return 1
}

echo "── Running from: $(pwd) ──"
echo "   Environment: $(if $IN_DOCKER; then echo 'Docker'; else echo 'Host'; fi)"
echo "   Git user:  $(git config user.name 2>/dev/null || echo '(not set)')"
echo "   Git email: $(git config user.email 2>/dev/null || echo '(not set)')"
echo "   GitHub:    $(gh api user --jq .login 2>/dev/null || echo '(not authenticated)')"
if select_remote; then
  echo "   Remote:    $SELECTED_REMOTE"
else
  echo "   Remote:    (none reachable)"
fi
echo ""

echo "── Git access checks ──"
check "git is installed"          git --version
check "inside a git repo"        git rev-parse --is-inside-work-tree
check "origin/upstream exists"   bash -lc 'git remote get-url origin >/dev/null 2>&1 || git remote get-url upstream >/dev/null 2>&1'
check "can fetch from a remote"  select_remote

echo ""
echo "── GitHub CLI checks ──"
check "gh is installed"           gh --version
check "gh is authenticated"       gh auth status
check "can list PRs on repo"      gh pr list --repo zakstak/kley --limit 1

echo ""
echo "── Rust toolchain ──"
check "cargo is installed"        cargo --version
check "cargo fmt available"       cargo fmt --version
check "cargo clippy available"    cargo clippy --version
check "kley binary works"        kley --help

echo ""
echo "── Dev toolchain checks ──"
optional "gcc is installed"           gcc --version
optional "make is installed"          make --version
optional "cmake is installed"         cmake --version
optional "node is installed"          node --version
optional "npm is installed"           npm --version
optional "go is installed"            go version
optional "python3 is installed"       python3 --version
optional "sqlite3 is installed"       sqlite3 --version
optional "shellcheck is installed"    shellcheck --version
optional "tree is installed"          tree --version
optional "jq is installed"            jq --version
optional "fd is installed"            fd --version
optional "bat is installed"           bat --version

echo ""
echo "── LSPs ──"
optional "rust-analyzer"              rust-analyzer --version
optional "gopls"                      gopls version
optional "typescript-language-server" typescript-language-server --version
optional "bash-language-server"       bash-language-server --version
optional "yaml-language-server"       yaml-language-server --version

echo ""
echo "── Linters & Formatters ──"
optional "golangci-lint"              golangci-lint --version
optional "prettier"                   prettier --version
optional "gitleaks"                   gitleaks version
optional "tsgo"                       tsgo --version
optional "cargo-nextest"              cargo nextest --version

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━"
echo "  Passed: $PASS  Failed: $FAIL  Warnings: $WARN"
echo "━━━━━━━━━━━━━━━━━━━━━━"

if [ "$FAIL" -gt 0 ]; then
  echo ""
  echo "⚠ Fix the failing checks above before running self-improve.sh"
  exit 1
else
  echo ""
  echo "✓ All checks passed — ready to self-improve!"
fi
