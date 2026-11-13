#!/usr/bin/env bash
# Check that Git does not ignore the plan directory.
# Check that Git ignores the rest of .opencode.
set -euo pipefail

readonly PLAN_FILE=".opencode/plan/specs.md"
readonly OTHER_FILE=".opencode/scratch.txt"

root="$(git rev-parse --show-toplevel)"
cd "$root"

if git check-ignore -q -- "$PLAN_FILE"; then
  echo "FAIL: Git ignores the plan file." >&2
  git check-ignore -v -- "$PLAN_FILE" >&2
  exit 1
fi

if ! git check-ignore -q -- "$OTHER_FILE"; then
  echo "FAIL: Git does not ignore other files in .opencode." >&2
  exit 1
fi

echo "PASS: Git does not ignore the plan directory."
