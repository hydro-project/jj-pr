#!/usr/bin/env bash
set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
BOLD='\033[1m'
RESET='\033[0m'

pass() { echo -e "${GREEN}✓${RESET} $1"; }
fail() { echo -e "${RED}✗${RESET} $1"; exit 1; }
step() { echo -e "\n${BOLD}▸ $1${RESET}"; }

step 'Prerequisites'
command -v git > /dev/null 2>&1 || fail 'Cannot find `git`'
command -v jj > /dev/null 2>&1 || fail 'Cannot find jujutsu `jj`. To install, run `cargo install --locked jj-cli --bin jj`'
pass 'All prerequisite executables found'

export RUST_BACKTRACE=1

step "Formatting"
# Suppress diff output so AI agents run `cargo fmt` instead of manually applying each diff.
cargo fmt --all --check > /dev/null 2>&1 || fail "formatting issues found (run 'cargo +nightly fmt --all' to fix)"
pass "All code is formatted"

step "Clippy (warnings denied)"
cargo clippy --all-targets -- -D warnings || fail "clippy warnings found"
pass "No clippy warnings"

step "Check"
cargo check --all-targets || fail "test targets failed to compile"
pass "Test targets compile"

step "Tests"
cargo test --all-targets || fail "tests failed"
pass "All tests passed"

echo -e "\n${GREEN}${BOLD}All checks passed.${RESET}"
