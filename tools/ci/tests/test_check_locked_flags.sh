#!/usr/bin/env bash
# test_check_locked_flags.sh - unit tests for check_locked_flags.sh.
#
# Builds small synthetic workflow trees under a tempdir, points the gate at
# them via OC_RSYNC_LOCKED_SCAN_ROOTS, and asserts the expected exit code for
# each case. This catches regressions in the gate's matcher (e.g., a future
# refactor that silently stops recognising `cargo nextest run` as a gated
# subcommand, or one that mis-classifies `cargo fmt` as gated).

set -uo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
target="${script_dir}/../check_locked_flags.sh"

if [[ ! -x "${target}" ]] && [[ ! -r "${target}" ]]; then
    echo "FAIL: cannot locate check_locked_flags.sh at ${target}" >&2
    exit 2
fi

tmp=$(mktemp -d)
trap 'rm -rf "${tmp}"' EXIT

failures=0
case_count=0

# Run the gate against a fixture directory and assert exit code.
#
# $1 - human-readable label
# $2 - expected exit code (0 = pass, 1 = violation)
# $3 - fixture directory under ${tmp}
run_case() {
    local label="$1" expected="$2" root="$3"
    case_count=$((case_count + 1))
    OC_RSYNC_LOCKED_SCAN_ROOTS="${root}" \
        bash "${target}" >"${tmp}/out" 2>"${tmp}/err"
    local actual=$?
    if [[ "${actual}" -ne "${expected}" ]]; then
        echo "FAIL ${label}: expected exit ${expected}, got ${actual}" >&2
        sed 's/^/  out: /' "${tmp}/out" >&2
        sed 's/^/  err: /' "${tmp}/err" >&2
        failures=$((failures + 1))
    else
        echo "ok ${label} (exit ${actual})"
    fi
}

# -------- Case 1: missing --locked on cargo build should fail (exit 1).
mkdir -p "${tmp}/case1"
cat >"${tmp}/case1/bad.yml" <<'YAML'
name: bad
on: [pull_request]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: cargo build --release --workspace
YAML
run_case "missing --locked on cargo build is rejected" 1 "${tmp}/case1"

# -------- Case 2: cargo build with --locked should pass (exit 0).
mkdir -p "${tmp}/case2"
cat >"${tmp}/case2/good.yml" <<'YAML'
name: good
on: [pull_request]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: cargo build --locked --release --workspace
YAML
run_case "cargo build --locked passes" 0 "${tmp}/case2"

# -------- Case 3: cargo fmt is lenient even without --locked.
mkdir -p "${tmp}/case3"
cat >"${tmp}/case3/fmt.yml" <<'YAML'
name: fmt
on: [pull_request]
jobs:
  fmt:
    runs-on: ubuntu-latest
    steps:
      - run: cargo fmt --all -- --check
YAML
run_case "cargo fmt without --locked is lenient" 0 "${tmp}/case3"

# -------- Case 4: cargo update without --locked is lenient (lockfile-sync).
mkdir -p "${tmp}/case4"
cat >"${tmp}/case4/sync.yml" <<'YAML'
name: sync
on: [pull_request]
jobs:
  sync:
    runs-on: ubuntu-latest
    steps:
      - run: cargo update --workspace
YAML
run_case "cargo update without --locked is lenient" 0 "${tmp}/case4"

# -------- Case 5: backslash-continued cargo nextest run --locked passes.
mkdir -p "${tmp}/case5"
cat >"${tmp}/case5/multiline.yml" <<'YAML'
name: multiline
on: [pull_request]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - run: |
          cargo nextest run \
            --locked \
            -p engine \
            --no-fail-fast
YAML
run_case "multi-line cargo nextest run --locked passes" 0 "${tmp}/case5"

# -------- Case 6: multi-line cargo nextest run WITHOUT --locked fails.
mkdir -p "${tmp}/case6"
cat >"${tmp}/case6/multiline_bad.yml" <<'YAML'
name: multiline_bad
on: [pull_request]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - run: |
          cargo nextest run \
            -p engine \
            --no-fail-fast
YAML
run_case "multi-line cargo nextest run missing --locked fails" 1 "${tmp}/case6"

# -------- Case 7: cargo clippy without --locked fails.
mkdir -p "${tmp}/case7"
cat >"${tmp}/case7/clippy.yml" <<'YAML'
name: clippy
on: [pull_request]
jobs:
  clippy:
    runs-on: ubuntu-latest
    steps:
      - run: cargo clippy --workspace --all-targets -- -D warnings
YAML
run_case "cargo clippy without --locked fails" 1 "${tmp}/case7"

# -------- Case 8: cargo check --locked in a shell helper script passes.
mkdir -p "${tmp}/case8"
cat >"${tmp}/case8/helper.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
cargo check --locked --workspace --all-features
SH
chmod +x "${tmp}/case8/helper.sh"
run_case "cargo check --locked in helper.sh passes" 0 "${tmp}/case8"

# -------- Case 9: a quoted `cargo build` inside an error message does
# not trip the gate.
mkdir -p "${tmp}/case9"
cat >"${tmp}/case9/quoted.yml" <<'YAML'
name: quoted
on: [pull_request]
jobs:
  notice:
    runs-on: ubuntu-latest
    steps:
      - run: echo "If 'cargo build' fails, rerun with --locked"
YAML
run_case "quoted 'cargo build' in an echo is ignored" 0 "${tmp}/case9"

# -------- Case 10: `cargo nextest list` (non-`run`) without --locked is
# lenient; only `cargo nextest run` is gated.
mkdir -p "${tmp}/case10"
cat >"${tmp}/case10/list.yml" <<'YAML'
name: list
on: [pull_request]
jobs:
  list:
    runs-on: ubuntu-latest
    steps:
      - run: cargo nextest list -p engine
YAML
run_case "cargo nextest list without --locked is lenient" 0 "${tmp}/case10"

# -------- Case 11: cargo run --locked passes; without --locked fails.
mkdir -p "${tmp}/case11a"
cat >"${tmp}/case11a/run_ok.yml" <<'YAML'
name: run_ok
on: [pull_request]
jobs:
  smoke:
    runs-on: ubuntu-latest
    steps:
      - run: cargo run --locked --release -- --version
YAML
run_case "cargo run --locked passes" 0 "${tmp}/case11a"

mkdir -p "${tmp}/case11b"
cat >"${tmp}/case11b/run_bad.yml" <<'YAML'
name: run_bad
on: [pull_request]
jobs:
  smoke:
    runs-on: ubuntu-latest
    steps:
      - run: cargo run --release -- --version
YAML
run_case "cargo run without --locked fails" 1 "${tmp}/case11b"

# -------- Case 12: a comment containing `cargo build` does not trip.
mkdir -p "${tmp}/case12"
cat >"${tmp}/case12/commented.yml" <<'YAML'
name: commented
on: [pull_request]
# this would have run cargo build --release without --locked
jobs:
  noop:
    runs-on: ubuntu-latest
    steps:
      - run: echo skipped
YAML
run_case "commented-out cargo build is ignored" 0 "${tmp}/case12"

echo
echo "ran ${case_count} case(s); ${failures} failure(s)"
if [[ "${failures}" -ne 0 ]]; then
    exit 1
fi
exit 0
