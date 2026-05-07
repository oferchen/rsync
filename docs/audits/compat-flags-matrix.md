# Compatibility-Flags Interaction Matrix (Issue #2106)

Audit of CLI option interactions for `--checksum`, `--inplace`, `--partial`,
`--partial-dir`, `--whole-file`, `--append`, `--append-verify`,
`--delay-updates`, `--write-devices`, and related flags. Verifies oc-rsync
matches upstream rsync 3.4.1 mutual-exclusion rules.

Note: a separate `compat-flags-audit.md` covers the protocol-30+ wire
`compat_flags` bitfield and capability strings; this document covers
**CLI option-set validation** only.

## 1. Upstream rsync 3.4.1 validation block

Source: `target/interop/upstream-src/rsync-3.4.1/options.c`.

| Lines | Logic |
|------|-------|
| 706-710 | `--inplace` / `--no-inplace`, `--append`, `--append-verify`, `--no-append` parsing. `--append` -> `OPT_APPEND` token (1706: `am_server` increments, client sets to 1). `--append-verify` -> `append_mode = 2`. |
| 734-736 | `--whole-file` / `--no-whole-file` / `--no-W` set tri-state `whole_file`. |
| 762-764 | `--partial`, `--no-partial`, `--partial-dir <dir>`. |
| 2382-2393 | `if (append_mode)` -> reject `--whole-file > 0`; otherwise force `inplace = 1`. |
| 2395-2401 | `if (write_devices)` -> force `inplace = 1`. |
| 2403-2404 | `if (delay_updates && !partial_dir)` set `partial_dir = ".~tmp~"`. |
| 2406-2414 | `if (inplace)` (set by `--inplace`, `--append`, or `--write-devices`) -> reject `partial_dir` with message `"--<inplace|append> cannot be used with --<delay-updates|partial-dir>"`. The `delay_updates` branch is reached because line 2403 silently aliases `delay_updates` to `partial_dir = ".~tmp~"`. |
| 2422-2427 | `#ifdef !HAVE_FTRUNCATE` -> reject `--inplace` / `--append` outright on the platform. |

## 2. Mutual-exclusion rules upstream enforces

| Rule | Upstream message | Trigger |
|------|------------------|---------|
| R1 | `--append cannot be used with --whole-file` | `append_mode > 0 && whole_file > 0` |
| R2 | `--inplace cannot be used with --partial-dir` | `--inplace` + `--partial-dir <X>` |
| R3 | `--append cannot be used with --partial-dir` | `--append` + `--partial-dir <X>` |
| R4 | `--inplace cannot be used with --delay-updates` | `--inplace` + `--delay-updates` (delay-updates aliases to partial-dir) |
| R5 | `--append cannot be used with --delay-updates` | `--append` + `--delay-updates` |
| R6 | `--inplace is not supported on this <client/server>` | platform without `ftruncate` |
| R7 | implicit: `--write-devices` -> `inplace = 1` | so R2/R3/R4/R5 also fire when `--write-devices` is combined with `--partial-dir` / `--delay-updates` |
| R8 | implicit: `--append-verify` -> `append_mode = 2`, so R1 still applies as for plain `--append` |

`--checksum` (`-c`) has **no** mutual-exclusion rules with these flags upstream.
`--ignore-times`, `--size-only`, `--existing`, `--ignore-existing` are
similarly orthogonal.

## 3. oc-rsync's enforcement

Source: `crates/core/src/client/config/builder/mod.rs` (`validate()` at
lines 268-295) and `crates/core/src/client/config/builder/partials.rs`.

| Rule | Enforced? | Site | Notes |
|------|-----------|------|-------|
| R1 (`--append` vs `--whole-file`) | NO | n/a | not detected at config-build time. `crates/core/src/client/remote/flags.rs:96-98` gates the wire `-W` flag with `whole_file && !append`, but no error is raised. |
| R2 (`--inplace` vs `--partial-dir`) | YES | `validate()` line 287-292 | error message `"--inplace cannot be used with --partial-dir"`. |
| R3 (`--append` vs `--partial-dir`) | YES | same | append routes through `is_inplace`; emits `"--append cannot be used with --partial-dir"`. |
| R4 (`--inplace` vs `--delay-updates`) | YES | `validate()` line 280-285 | message `"--inplace cannot be used with --delay-updates"`. Upstream emits `"... cannot be used with --partial-dir"` because delay_updates is aliased to `.~tmp~`. **String mismatch**. |
| R5 (`--append` vs `--delay-updates`) | YES | same | same caveat as R4. |
| R6 (no-ftruncate platform) | N/A | unused | every supported platform has truncation; oc-rsync does not emit this error. |
| R7 (`--write-devices` aliasing) | NO | n/a | `crates/core/src/client/config/builder/preservation.rs:50-53` sets `write_devices` but does not force `inplace = true`. `--write-devices --partial-dir X` passes `validate()`; upstream rejects it. |
| R8 (`--append-verify` -> append) | YES | `partials.rs:65-72` | `append_verify(true)` sets `append = true`, so all `append` rules apply transitively. |

Tests confirming R2-R5: `crates/core/src/client/config/builder/tests.rs`
lines 1058-1115.

## 4. Gaps

1. **R1 silently accepted** (`--append --whole-file`): both flags coexist in `ClientConfig`. Remote-flag synthesis suppresses the wire `-W` when append is set, but no `ConfigConflict` is raised. Upstream prints `--append cannot be used with --whole-file` and exits with `RERR_SYNTAX` (1).
2. **R7 silently accepted** (`--write-devices` + `--partial-dir` / `--delay-updates`): upstream forces `inplace = 1` first, so these combinations are rejected. oc-rsync passes validation, then runs with both flags active.
3. **R4/R5 wording**: oc-rsync emits `"... --delay-updates"`; upstream emits `"... --partial-dir"` due to the silent alias. More accurate, but breaks string-identity interop for stderr-grepping tools.
4. `validate()` is invoked from the build pipeline but the surface is internal; the CLI front-end (`crates/cli/src/frontend/arguments/parsed_args/mod.rs`) does not pre-validate, so help text / `--dry-run --debug` paths can defer the conflict report.

## 5. Plan: `cli_validation_matrix.rs`

Add an integration test at
`crates/core/tests/cli_validation_matrix.rs` driven by a single
`(flags, expected)` table (>= 30 rows). Categories:

- 5 rows for R2 (`--inplace` + `--partial-dir <X>`, with/without `--partial`, with empty/non-empty `<X>`).
- 5 rows for R3 (`--append`, `--append-verify` x `--partial-dir`).
- 4 rows for R4/R5 (`--inplace`, `--append`, `--append-verify` x `--delay-updates`).
- 4 rows for R1 (`--append --whole-file`, `--append-verify --whole-file`, `--append --no-whole-file`, `--append --no-W`) - **currently failing**, drives fix.
- 4 rows for R7 (`--write-devices --partial-dir`, `--write-devices --delay-updates`) - **currently failing**.
- 4 rows for accepted orthogonal combos (`--checksum --inplace`, `--checksum --append`, `--size-only --partial-dir`, `--ignore-times --whole-file`) - must pass.
- 4 rows verifying default builders (`--inplace` alone, `--append` alone, `--partial-dir` alone, `--delay-updates` alone) - must pass.

Each row asserts:

- `Ok(())` for permitted combinations.
- `Err(ConfigConflict { option1, option2 })` with **exact** `option1`/`option2` matching upstream wording (R4/R5 rewritten to emit `"partial-dir"` once the alias is added).

Follow-up (separate PRs):

- Fix R1: extend `validate()` to detect `self.append && self.whole_file == Some(true)`.
- Fix R7: alias `write_devices -> inplace = true` in `preservation.rs::write_devices` (mirror `options.c:2395-2401`).
- Re-align R4/R5 messages to upstream wording or document the deviation in `docs/audits/error-format-upstream-comparison.md`.
