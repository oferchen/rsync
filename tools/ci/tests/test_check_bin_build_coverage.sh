#!/usr/bin/env bash
# test_check_bin_build_coverage.sh - unit tests for check_bin_build_coverage.py.
#
# Builds synthetic workflow files under a tempdir, points the guard at them
# with --workflows, and asserts the exit code and the reported cell for each
# case. A guard that cannot be shown to fail is worthless: these cases pin
# both directions, so a refactor that silently stops recognising
# `cargo nextest run`, matrix `include` rows, or a `needs_bin` conditional
# surfaces here instead of as a vacuous pass on a future PR.

set -uo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
target="${script_dir}/../check_bin_build_coverage.py"

if [[ ! -r "${target}" ]]; then
    echo "FAIL: cannot locate check_bin_build_coverage.py at ${target}" >&2
    exit 2
fi

tmp=$(mktemp -d)
trap 'rm -rf "${tmp}"' EXIT

failures=0
cases=0

# Run the guard over a single synthetic workflow and assert on its outcome.
#   expect_case <name> <expected-exit> <expected-substring-or-empty> <yaml>
expect_case() {
    local name=$1 expected_exit=$2 expected_text=$3 body=$4
    local dir="${tmp}/${name}"
    cases=$((cases + 1))
    mkdir -p "${dir}"
    printf '%s\n' "${body}" >"${dir}/synthetic.yml"

    local output status
    output=$(python3 "${target}" --workflows "${dir}" 2>&1)
    status=$?

    if [[ ${status} -ne ${expected_exit} ]]; then
        echo "FAIL(${name}): exit ${status}, expected ${expected_exit}" >&2
        printf '%s\n' "${output}" >&2
        failures=$((failures + 1))
        return
    fi
    if [[ -n ${expected_text} ]] && ! grep -qF -- "${expected_text}" <<<"${output}"; then
        echo "FAIL(${name}): output does not mention '${expected_text}'" >&2
        printf '%s\n' "${output}" >&2
        failures=$((failures + 1))
        return
    fi
    echo "ok(${name})"
}

# A package-scoped run over a resolver crate with no build step is the exact
# failure mode the guard exists for.
expect_case uncovered 1 'job:    tests' 'name: t
on: [push]
jobs:
  tests:
    runs-on: ubuntu-latest
    steps:
      - name: Run
        run: cargo nextest run --locked -p engine'

# Same cell, with the build step: covered.
expect_case covered 0 'ok: every test cell' 'name: t
on: [push]
jobs:
  tests:
    runs-on: ubuntu-latest
    steps:
      - name: Build oc-rsync
        run: cargo build --locked -p bin --bin oc-rsync
      - name: Run
        run: cargo nextest run --locked -p engine'

# Order matters: a build placed after the test step does not cover it.
expect_case build_after_test 1 'reason: no preceding step builds oc-rsync' 'name: t
on: [push]
jobs:
  tests:
    runs-on: ubuntu-latest
    steps:
      - name: Run
        run: cargo nextest run --locked -p engine
      - name: Build oc-rsync
        run: cargo build --locked -p bin --bin oc-rsync'

# `--workspace` builds the root bin package itself.
expect_case workspace_selection 0 'ok: every test cell' 'name: t
on: [push]
jobs:
  tests:
    runs-on: ubuntu-latest
    steps:
      - name: Run
        run: cargo nextest run --locked --workspace --all-features'

# A workspace build earlier in the job covers a later package-scoped run.
expect_case workspace_build_covers 0 'ok: every test cell' 'name: t
on: [push]
jobs:
  tests:
    runs-on: ubuntu-latest
    steps:
      - name: Build
        run: cargo build --locked --workspace
      - name: Run
        run: cargo nextest run --locked -p transfer'

# Crates that never touch the resolver need no build step.
expect_case non_resolver_crate 0 'ok: every test cell' 'name: t
on: [push]
jobs:
  tests:
    runs-on: ubuntu-latest
    steps:
      - name: Run
        run: cargo nextest run --locked -p checksums -p protocol'

# Matrix rows are expanded: only the row without needs_bin is reported.
# shellcheck disable=SC2016  # `${{ }}` is workflow syntax, not shell.
expect_case matrix_needs_bin 1 'row:    name=bad, args=-p core, needs_bin=False' 'name: t
on: [push]
jobs:
  tests:
    runs-on: ubuntu-latest
    strategy:
      matrix:
        include:
          - name: good
            args: "-p core"
            needs_bin: true
          - name: bad
            args: "-p core"
            needs_bin: false
    steps:
      - name: Build oc-rsync
        if: matrix.needs_bin
        run: cargo build --locked -p bin --bin oc-rsync
      - name: Run
        run: cargo nextest run --locked --profile ci ${{ matrix.args }}'

# A build at a different cargo profile lands in a different directory, so it
# does not cover a debug-profile test run.
expect_case profile_mismatch 1 'reason: no preceding step builds oc-rsync' 'name: t
on: [push]
jobs:
  tests:
    runs-on: ubuntu-latest
    steps:
      - name: Build oc-rsync (release)
        run: cargo build --locked --release -p bin --bin oc-rsync
      - name: Run
        run: cargo nextest run --locked -p metadata'

# A guard that analyses nothing must not report success.
mkdir -p "${tmp}/empty"
cases=$((cases + 1))
if python3 "${target}" --workflows "${tmp}/empty" >/dev/null 2>&1; then
    echo "FAIL(empty_dir): guard passed with no workflows to analyse" >&2
    failures=$((failures + 1))
else
    echo "ok(empty_dir)"
fi

# The crate list must be derived from the tree, not pinned in the script.
#
# The negatives are pure libraries that never spawn a subprocess, so they can
# only appear if the derivation has broken. `test-support` is deliberately NOT
# among them: it is the provider, and `derive_resolver_packages` exempts only a
# provider's `src/`, counting any `tests/` reference as a real call site. Its own
# integration tests name `OcRsyncCliRunner`, so it joins the set by that rule -
# a conservative classification, since being in the set only ever *demands* a
# build step, never omits one.
cases=$((cases + 1))
crates=$(python3 "${target}" --list-crates)
if grep -qE '^(cli|core|engine|metadata|transfer)$' <<<"${crates}" &&
    ! grep -qE '^(checksums|protocol)$' <<<"${crates}"; then
    echo "ok(derived_crates)"
else
    echo "FAIL(derived_crates): unexpected crate list:" >&2
    printf '%s\n' "${crates}" >&2
    failures=$((failures + 1))
fi

echo "ran ${cases} case(s), ${failures} failure(s)"
[[ ${failures} -eq 0 ]]
