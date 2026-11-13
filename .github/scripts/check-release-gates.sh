#!/usr/bin/env bash
#
# Check that the release gates are wired into CI.
#
# The release must require the browser tier and the qualification matrix. The
# browser tier must run Chrome and Firefox. The qualification workflow must run
# the freeze gate, the receipt gate, and the vector immutability gate.
set -uo pipefail

CI=.github/workflows/ci.yml
RELEASE=.github/workflows/release.yml
QUALIFICATION=.github/workflows/qualification.yml

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

[ -f "$CI" ] || fail "missing $CI"
[ -f "$RELEASE" ] || fail "missing $RELEASE"
[ -f "$QUALIFICATION" ] || fail "missing $QUALIFICATION"

grep -Fq 'uses: ./.github/workflows/ci.yml' "$RELEASE" \
  || fail "$RELEASE does not call $CI"

grep -Fq 'wasm-browser:' "$CI" \
  || fail "$CI has no wasm-browser job"

grep -Fq 'browser: Chrome' "$CI" \
  || fail "$CI has no Chrome matrix leg"

grep -Fq 'browser: Firefox' "$CI" \
  || fail "$CI has no Firefox matrix leg"

grep -Fq -- '--chrome' "$CI" \
  || fail "$CI does not run wasm-pack with headless Chrome"

grep -Fq -- '--firefox' "$CI" \
  || fail "$CI does not run wasm-pack with headless Firefox"

grep -Fq 'firefox-esr' "$CI" \
  || fail "$CI does not install Firefox"

grep -Fq 'geckodriver' "$CI" \
  || fail "$CI does not install geckodriver"

grep -Fq 'qualification:' "$CI" \
  || fail "$CI has no qualification job"

grep -Fq 'cargo run -p oxkv-qualify' "$CI" \
  || fail "$CI does not run the qualification suite"

grep -Fq 'cargo test -p oxkv-qualify --locked --test immutability' "$QUALIFICATION" \
  || fail "$QUALIFICATION does not run the vector immutability test"

grep -Fq 'cargo test -p oxkv-freeze --locked' "$QUALIFICATION" \
  || fail "$QUALIFICATION does not run the freeze tests"

grep -Fq 'cargo test -p oxkv-manifest --locked' "$QUALIFICATION" \
  || fail "$QUALIFICATION does not run the receipt tests"

grep -Fq 'cargo test -p oxkv-qualify --locked --test release_gate' "$QUALIFICATION" \
  || fail "$QUALIFICATION does not run the register agreement test"

printf 'PASS: the release gates include the Chrome and Firefox browser tiers and the qualification matrix.\n'
