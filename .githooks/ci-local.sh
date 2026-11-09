#!/usr/bin/env bash
# Run the GitHub Actions CI jobs on this machine.
set -uo pipefail

readonly DEFAULT_JOBS=(native wasm deps release-config)
readonly ALL_JOBS=(native wasm wasm-browser deps release-config)
readonly SKIP_STATUS=77

usage() {
  cat <<'EOF'
Usage: .githooks/ci-local.sh [options] [job...]

Run the GitHub Actions CI jobs on this machine. The script does not
download toolchains or targets.

Jobs:
  native          Format check, clippy, tests, and docs
  wasm            wasm32 tests, check, and clippy
  wasm-browser    wasm32 tests in headless Chrome
  deps            cargo-deny licence, advisory, and ban checks
  release-config  Verify that the release workflow gates on CI

Options:
  --all           Run every job, including wasm-browser
  --list          List the job names
  -h, --help      Show this help

Environment:
  CI_LOCAL_STRICT=1  Turn every skipped job into a failure

With no job names, the script runs: native wasm deps release-config
EOF
}

have() {
  command -v "$1" >/dev/null 2>&1
}

skip() {
  local job="$1"
  shift
  echo "SKIP: $job: $*"
  return "$SKIP_STATUS"
}

require_tool() {
  local job="$1"
  local tool="$2"
  local install="$3"
  have "$tool" || skip "$job" "missing required tool '$tool'. Install it with: $install"
}

require_wasm_target() {
  local job="$1"
  if have rustup; then
    if rustup target list --installed 2>/dev/null | grep -qx 'wasm32-unknown-unknown'; then
      return 0
    fi
  else
    local sysroot
    sysroot="$(rustc --print sysroot 2>/dev/null || true)"
    if [[ -n "$sysroot" && -d "$sysroot/lib/rustlib/wasm32-unknown-unknown" ]]; then
      return 0
    fi
  fi
  skip "$job" "the wasm32-unknown-unknown target is not installed. Install it with: rustup target add wasm32-unknown-unknown"
}

job_native() {
  require_tool native cargo "mise install"
  require_tool native cargo-fmt "rustup component add rustfmt"
  require_tool native cargo-clippy "rustup component add clippy"

  cargo fmt --check
  cargo clippy --all-targets -- -D warnings
  cargo test
  cargo clippy --all-targets --all-features -- -D warnings
  cargo test --all-features
  RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
}

job_wasm() {
  require_tool wasm cargo "mise install"
  require_tool wasm wasm-pack "cargo install wasm-pack"
  require_tool wasm node "mise install"
  require_wasm_target wasm

  wasm-pack test --node
  cargo check --target wasm32-unknown-unknown --all-features
  cargo clippy --target wasm32-unknown-unknown --all-features -- -D warnings
}

job_deps() {
  require_tool deps cargo "mise install"
  require_tool deps cargo-deny "cargo install --locked cargo-deny --version 0.20.2"

  cargo deny check
}

job_wasm_browser() {
  require_tool wasm-browser cargo "mise install"
  require_tool wasm-browser wasm-pack "cargo install wasm-pack"
  require_tool wasm-browser node "mise install"
  require_tool wasm-browser chromedriver "apt-get install chromium-driver"
  require_wasm_target wasm-browser

  local chrome=""
  local candidate
  for candidate in chromium chromium-browser google-chrome google-chrome-stable; do
    if have "$candidate"; then
      chrome="$candidate"
      break
    fi
  done
  if [[ -z "$chrome" ]]; then
    skip wasm-browser "missing Chrome/Chromium. Install it with: apt-get install chromium"
  fi

  wasm-pack test --headless --chrome
}

release_job_block() {
  local job="$1"
  awk -v job="$job" '
    $0 ~ ("^  " job ":[[:space:]]*$") { inside = 1; print; next }
    inside && $0 ~ /^  [A-Za-z0-9_.-]+:[[:space:]]*$/ { exit }
    inside { print }
  ' .github/workflows/release.yml
}

release_job_needs() {
  release_job_block "$1" | awk '
    /^[[:space:]]+needs:/ {
      value = $0
      sub(/^[[:space:]]+needs:[[:space:]]*/, "", value)
      if (value != "") {
        print value
      }
      in_block = (value == "")
      next
    }
    in_block && /^[[:space:]]+-[[:space:]]*/ {
      item = $0
      sub(/^[[:space:]]+-[[:space:]]*/, "", item)
      print item
      next
    }
    in_block { in_block = 0 }
  '
}

needs_contains() {
  local needs="$1"
  local dependency="$2"
  printf '%s\n' "$needs" \
    | tr -d '[]' \
    | tr ',' '\n' \
    | sed 's/^[[:space:]]*//; s/[[:space:]]*$//' \
    | grep -qxF -- "$dependency"
}

job_release_config() {
  local file=".github/workflows/release.yml"

  if [[ ! -f "$file" ]]; then
    echo "FAIL: release-config: missing $file." >&2
    return 1
  fi

  if ! grep -Fq 'uses: ./.github/workflows/ci.yml' "$file"; then
    echo "FAIL: release-config: $file does not call .github/workflows/ci.yml." >&2
    return 1
  fi

  local needs
  needs="$(release_job_needs verify-publish)"
  if ! needs_contains "$needs" checks; then
    echo "FAIL: release-config: verify-publish does not need checks." >&2
    return 1
  fi

  needs="$(release_job_needs publish-crate)"
  if ! needs_contains "$needs" checks || ! needs_contains "$needs" verify-publish; then
    echo "FAIL: release-config: publish-crate does not need both checks and verify-publish." >&2
    return 1
  fi

  needs="$(release_job_needs publish-npm)"
  if ! needs_contains "$needs" checks || ! needs_contains "$needs" verify-publish; then
    echo "FAIL: release-config: publish-npm does not need both checks and verify-publish." >&2
    return 1
  fi

  echo "PASS: release workflow gates publishing on CI checks."
}

run_job() {
  (
    set -euo pipefail
    case "$1" in
      native) job_native ;;
      wasm) job_wasm ;;
      wasm-browser) job_wasm_browser ;;
      deps) job_deps ;;
      release-config) job_release_config ;;
    esac
  )
}

main() {
  local list=0
  local all=0
  local selected=()
  local arg

  for arg in "$@"; do
    case "$arg" in
      --all) all=1 ;;
      --list) list=1 ;;
      -h | --help) usage; return 0 ;;
      -*)
        echo "error: unknown option: $arg" >&2
        usage >&2
        return 2
        ;;
      native | wasm | wasm-browser | deps | release-config) selected+=("$arg") ;;
      *)
        echo "error: unknown job: $arg" >&2
        usage >&2
        return 2
        ;;
    esac
  done

  if ((list)); then
    printf '%s\n' "${ALL_JOBS[@]}"
    return 0
  fi

  if ((all)); then
    selected=("${ALL_JOBS[@]}")
  elif ((${#selected[@]} == 0)); then
    selected=("${DEFAULT_JOBS[@]}")
  fi

  local root
  if ! root="$(git rev-parse --show-toplevel 2>/dev/null)" || [[ -z "$root" ]]; then
    echo "error: not inside a git repository." >&2
    return 1
  fi
  cd "$root" || return 1

  local job status
  for job in "${selected[@]}"; do
    echo "==> $job"
    run_job "$job"
    status=$?

    if ((status == 0)); then
      echo "PASS: $job"
    elif ((status == SKIP_STATUS)); then
      if [[ "${CI_LOCAL_STRICT:-0}" == "1" ]]; then
        echo "FAIL: job $job was skipped and CI_LOCAL_STRICT=1." >&2
        return 1
      fi
    else
      echo "FAIL: job $job failed with exit code $status." >&2
      return "$status"
    fi
  done

  return 0
}

main "$@"
