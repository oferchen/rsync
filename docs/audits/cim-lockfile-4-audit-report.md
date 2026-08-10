# CIM-LOCKFILE-4 audit report

## Summary

Audited every CI workflow and `tools/ci/` helper script for `cargo`
invocations missing the `--locked` flag. **Zero gaps detected.** Every
gated cargo invocation in CI honours `Cargo.lock` byte-for-byte.

The active regression guard `tools/ci/check_locked_flags.sh` (CIM-LOCKFILE-5,
landed via PR #5816) enforces this property on every PR via the
`Cargo.lock guard` workflow at `.github/workflows/check-locked-flags.yml`.

## Method

Ran the workspace audit script:

```sh
bash tools/ci/check_locked_flags.sh
```

The script joins backslash-continued lines, strips YAML/shell comments,
masks quoted spans, and matches `cargo` invocations of the gated
subcommands listed below. It is the same logic the regression-guard
workflow runs on every PR.

### Gated subcommands

A `cargo` invocation must carry `--locked` when its subcommand is one of:

- `cargo build`
- `cargo check`
- `cargo clippy`
- `cargo nextest run`
- `cargo run`
- `cargo test`

### Exempt subcommands (no `--locked` required)

- `cargo fmt` - does not consume lockfile.
- `cargo update` - lockfile-mutation entry point itself.
- `cargo doc`, `cargo bench`, `cargo tree`, `cargo install` - third-party
  or non-lockfile-consuming.
- `cargo xtask`, `cargo deb`, `cargo generate-rpm`, `cargo llvm-cov`,
  `cargo fuzz`, `cargo cov`, `cargo deny` - wrap an already-built
  artifact or are third-party plugins with their own semantics.

## Scope

The script's `SCAN_ROOTS` cover both surfaces where CI cargo invocations
live:

- `.github/workflows/` - 50 workflow YAMLs.
- `tools/ci/` - 17 shell helpers and supporting YAMLs.

Total scanned: **67 files** (`.yml`, `.yaml`, `.sh`).

## Result

```
=== CIM-LOCKFILE-5: --locked flag regression guard ===
Scanning workflows and tools/ci helpers for cargo invocations
of: build check clippy nextest run test

Scanned 67 file(s); inspected 73 gated cargo invocation(s).
PASSED: all gated cargo invocations carry --locked.
```

- **67** files scanned.
- **73** gated `cargo` invocations inspected.
- **0** violations found.
- **0** allowlist entries needed.

## Provenance

Prior CIM-LOCKFILE series PRs that drove the workspace to this clean
state:

- `CII-1.b` / `CII-1.e` - added `--locked` to interop workflows.
- `CII-1.f` - audited remaining `cargo build` / `cargo nextest` sites.
- PR #5816 (`chore(ci): gate against --locked removal from cargo invocations`)
  - shipped the regression-guard script and CI workflow.

This audit confirms no drift since PR #5816 landed.

## Continuous enforcement

`.github/workflows/check-locked-flags.yml` runs
`tools/ci/check_locked_flags.sh` on every PR touching workflow YAMLs or
`tools/ci/` scripts. A future PR that introduces `cargo build`,
`cargo nextest run`, or any other gated subcommand without `--locked`
fails the gate with an explicit `VIOLATION: path:line` diagnostic.

New intentional exceptions must be added to the script's `ALLOWLIST`
array with reviewer signoff, per the policy documented in CONTRIBUTING.md
section "Cargo.lock maintenance".

## Conclusion

CIM-LOCKFILE-4 closes clean. No workflow patches required.
