# BPF-4: EnvGuard CI lint specification for BufferPool cap-tests

## 1. Scope

BPF-4 specifies a CI-level lint that fires when a workspace test mutates
`BufferPool` capacity state - either through the `OC_RSYNC_BUFFER_POOL_*`
environment variables, raw `std::env::set_var` / `remove_var` against those
keys, or future cap-touching APIs on `BufferPool` - without holding an
`EnvGuard` for the duration of the mutation.

The lint is a stop-gap. The deeper fix is BPF-6..BPF-9, which replace the
global `OnceLock` singleton with a per-test factory that hands each test an
isolated `BufferPool` instance, removing the need for environment-variable
serialisation entirely. Once BPF-9 lands and the `OC_RSYNC_BUFFER_POOL_*`
env vars stop being the supported tuning surface in tests, this lint is
deleted. The script must carry an explicit EOL header pointing at BPF-9.

Cross-references (memory notes):

- `[[project_bufferpool_test_serialization_fragile]]` - the global `OnceLock`
  pool forces cap-tests to use a fragile `EnvGuard` pattern; the lint is
  the per-PR enforcement of that fragility until the singleton goes away.
- `[[project_bufferpool_count_cap]]` - byte-cap regression coverage is
  binding; the lint exists so future regressions cannot re-introduce a
  cap-touching test without serialisation.

Prior art in this series:

- BPF-1 (#2819) - inventoried every cap-touching test
  (`docs/audit/bufferpool-cap-tests-inventory.md`).
- BPF-2 (#2820) - classified BPF-1 results by `EnvGuard` coverage
  (`docs/audit/bufferpool-envguard-gap-list.md`).
- BPF-3 (#2821) - wraps the BPF-2 gap list with `EnvGuard`.
- BPF-5 (#2823) - implements this lint.
- BPF-6..BPF-9 - per-test factory; obsoletes `EnvGuard` and this lint.

## 2. What the lint detects

The lint walks every `#[test]` / `#[tokio::test]` function in the workspace
and inspects the function body for two sets of tokens.

### 2.1 Cap-touching tokens (the smell)

A function body matches if it contains any of the following literal byte
sequences:

- `OC_RSYNC_BUFFER_POOL_SIZE` - the only cap-affecting env var defined today
  (`crates/engine/src/local_copy/buffer_pool/global.rs:65`).
- `OC_RSYNC_BUFFER_POOL_` - prefix match catches future cap-affecting
  variables added without updating the lint (telemetry-only
  `OC_RSYNC_BUFFER_POOL_STATS` lives in this prefix and is explicitly
  ignored via 5.2 below).
- `init_global_buffer_pool` -
  `crates/engine/src/local_copy/buffer_pool/global.rs:132`. Mutates the
  process-wide `OnceLock`; running it in parallel with another cap-test
  poisons the singleton for the rest of the suite.
- `with_memory_cap`, `with_byte_budget`, `with_buffer_size`,
  `with_throughput_tracking`, `with_throughput_tracking_alpha`,
  `with_adaptive_resizing`, `with_buffer_controller`, `with_allocator` -
  the `BufferPool` builder methods at
  `crates/engine/src/local_copy/buffer_pool/pool.rs:232..409`. These
  mutate local instances, not the singleton; they are tracked because
  a test using them in combination with `global_buffer_pool()` is what
  caused the original BPF-1 race.
- `std::env::set_var(` and `std::env::remove_var(` - raw env mutation.
  Even when the key is unknown to the lint, raw env mutation in a test
  body without an `EnvGuard` is a smell.
- `unsafe { std::env::set_var` and `unsafe { std::env::remove_var` -
  the Rust 2024-edition unsafe-gated form. Same rationale.

### 2.2 Guard tokens (the proof)

A function body is considered guarded if it contains any of:

- `EnvGuard` - matches both the canonical guard at
  `crates/platform/src/env.rs:19` (`platform::env::EnvGuard`) and the
  inline duplicates listed below.
- `platform::env::EnvGuard` - fully-qualified canonical guard.

The inline duplicate guards (all functionally equivalent to the canonical
one) match on the bare `EnvGuard` token already:

- `crates/engine/src/local_copy/buffer_pool/global.rs:231`
- `crates/engine/src/concurrent_delta/spill/env.rs:130`
- `crates/engine/tests/spill_env_e2e.rs:54`
- `crates/branding/src/branding/tests.rs:43`
- `crates/cli/src/frontend/tests/common.rs:209`
- `crates/cli/src/frontend/arguments/env.rs:44`
- `crates/cli/tests/environment_variable_defaults.rs:11`
- `crates/core/src/client/config/compress_env.rs:75`
- `crates/core/src/client/tests/module_list_auth.rs:119`
- `crates/embedding/src/lib.rs:469`
- `crates/fast_io/tests/iouring_probe_fallback_mock.rs:36`
- `crates/rsync_io/tests/ssh_config_compression.rs:29`

BPF-8 will collapse these to a single re-export of
`platform::env::EnvGuard`; the lint does not need to know the difference.

### 2.3 Verdict

If any cap-touching token from 2.1 appears in a test body and no guard
token from 2.2 appears in the same body, the lint emits one violation
record and exits non-zero.

A representative test that passes the lint today:
`env_var_overrides_pool_size` at
`crates/engine/src/local_copy/buffer_pool/global.rs:273` - holds
`EnvGuard::set(ENV_BUFFER_POOL_SIZE, "42")` before constructing
`GlobalBufferPoolConfig::default()`.

## 3. Implementation choice

Three options were considered:

- (a) Custom `cargo xtask` lint that walks `crates/*/tests/` with
  `syn`-driven AST traversal. Typesafe; selects test functions by
  attribute rather than text. Cost: a new binary, slower CI step,
  ongoing maintenance for a script with a deletion date.
- (b) Shell + ripgrep lint script (`tools/ci/check_envguard.sh`). Walks
  the same files using regex matching with `rg -U` for multi-line
  context. Cost: regex-fragile if test bodies grow exotic indentation
  patterns; mitigated by the narrow token set.
- (c) Clippy-driven `internal_lint` proc-macro. Plugs into native rustc
  plumbing. Cost: heavy machinery for a single rule; clippy lints do
  not see test-file body semantics across modules cleanly.

**Decision: option (b).** Justification:

1. The lint is short-lived: BPF-9 deletes it once the per-test factory
   ships. Investing in (a) or (c) means the cost outlives the value.
2. The pattern surface is narrow - 8 cap-touching tokens, 2 guard tokens,
   all literal byte sequences. Regex matching is robust here.
3. `tools/ci/` already hosts comparable scripts
   (`check_known_failures.sh`, `run_interop.sh`); the lint slots into
   established conventions.
4. False positives are cheap to silence via the ignore file (4.2).
5. Linux-only execution matches the rest of `tools/ci/`; no Windows
   shell-portability work needed before deletion.

## 4. Script behaviour

### 4.1 Signature

```
tools/ci/check_envguard.sh [--list-tracked] [--ignore-file <path>]
```

### 4.2 Flags

- `--list-tracked` - prints the current set of cap-touching tokens
  (section 2.1) one per line, then exits 0. Reviewers verify the lint
  scope by running `bash tools/ci/check_envguard.sh --list-tracked`.
- `--ignore-file <path>` - opt-out list for tests that intentionally
  do not need `EnvGuard`. Defaults to `tools/ci/envguard_lint.ignore`.
  Format: one entry per line as `<repo-relative-path>::<test_fn_name>`;
  blank lines and `#` comments allowed. The single canonical entry on
  day one is the `EnvGuard` self-test in
  `crates/platform/src/env.rs::set_restores_on_drop` and its siblings,
  which intentionally exercise raw `std::env::set_var` to prove the
  guard restores state on drop.

### 4.3 Exit codes

- `0` - no violations, or `--list-tracked` invoked.
- `1` - at least one violation. One line per violation on stderr in the
  format:

  ```
  <repo-rel-path>:<line>  fn <test_name>  matched <CAP_TOKEN>
  ```

  Followed by a single summary line on stderr:
  `EnvGuard lint: N violation(s) - see https://github.com/.../issues/2822`.
- `2` - script invocation error (missing `rg`, unreadable ignore file,
  unknown flag).

## 5. Lint scope

### 5.1 Walked paths

- `crates/*/tests/**/*.rs`
- `crates/*/src/**/*.rs` (filtered to files containing `#[cfg(test)]` or
  `#[test]` so the lint does not parse production-only modules)

### 5.2 Excluded paths

- `target/`
- `.claude/worktrees/`
- `tools/` (this directory hosts the lint itself; tests here are not
  cap-touching)
- `xtask/`
- `fuzz/` (no test functions touch the buffer pool singleton)
- Any path matched by the ignore file (4.2)

`OC_RSYNC_BUFFER_POOL_STATS` is a read-only telemetry knob. It is
matched by the `OC_RSYNC_BUFFER_POOL_` prefix in 2.1, so any test
that references it must either also reference `EnvGuard` or appear in
the ignore file. This is intentional - reading the telemetry knob
without serialisation is still racy against another test that sets it.

## 6. Detection algorithm

```text
1. resolve --ignore-file (default: tools/ci/envguard_lint.ignore)
   load entries as "<path>::<fn_name>" tuples
2. for each candidate file under section 5.1, not in 5.2:
     parse with `rg -U --multiline-dotall`
     extract every span beginning at a #[test] or #[tokio::test] attribute
     and ending at the brace-balanced closing `}` of the following fn
     (regex with brace-counting in awk; rg captures the start line)
3. for each (file, line, fn_name, body) tuple:
     if (file, fn_name) in ignore set: skip
     if body matches any CAP_TOKEN from 2.1:
       if body matches any GUARD_TOKEN from 2.2: skip
       else:
         emit "file:line  fn fn_name  matched FIRST_CAP_TOKEN" to stderr
         violations += 1
4. if violations > 0:
     emit summary to stderr
     exit 1
   else:
     exit 0
```

Brace balancing is a single awk pass over the file: increment on `{`,
decrement on `}`, emit the span when depth returns to zero. String
literals and comments are not parsed; the cap-tokens are literal enough
that quoting them inside a Rust string still constitutes a real
violation worth surfacing (the test still mutates the env in practice).

## 7. CI integration

### 7.1 Workflow location

Add a step to the existing `lint` job in `.github/workflows/ci.yml`
(currently `fmt + clippy`, runs-on `ubuntu-latest`, timeout 10 minutes).
The lint shares the same runner, so it costs no extra job startup.

```yaml
      - name: EnvGuard lint
        run: bash tools/ci/check_envguard.sh
```

Insert the step after `Clippy` and before any subsequent step (today
nothing follows; future additions go after this).

### 7.2 Required-check status

The step inherits the `lint` job's required-check status. `lint` is
already a required check for merge (CI: fmt+clippy in the project rule
set), so no branch-protection edit is needed.

### 7.3 Platform coverage

Linux only. The lint is filesystem-pure: it walks tracked files and
greps for byte sequences. No platform variance is possible, and adding
the lint to the `Windows` or `macOS` matrix jobs would double-charge CI
time for zero additional signal.

## 8. Acceptance criteria for BPF-5 implementation

BPF-5 (#2823) is the implementation task. It is complete when:

1. `tools/ci/check_envguard.sh` exists, is executable, and runs on
   `ubuntu-latest` in under 5 seconds against the full workspace.
2. `bash tools/ci/check_envguard.sh` exits 0 on master **after** BPF-3
   has merged. (BPF-5 may not land before BPF-3; otherwise the lint
   fails the very job that introduces it.)
3. `bash tools/ci/check_envguard.sh` exits 1 on a synthetic violation -
   for example a fixture test that calls
   `std::env::set_var("OC_RSYNC_BUFFER_POOL_SIZE", "1")` without
   `EnvGuard`. BPF-5 ships this fixture under `tools/ci/fixtures/`
   and unit-tests the script against it.
4. `bash tools/ci/check_envguard.sh --list-tracked` prints the eight
   cap-touching tokens from 2.1, one per line, exits 0.
5. `bash tools/ci/check_envguard.sh --ignore-file /dev/null` runs the
   lint against the workspace with no ignore entries; this must still
   exit 0 once the `EnvGuard` self-test is the only legitimate
   raw-env-mutation site and the ignore file is the only thing
   silencing it. (If the self-test must be ignored to pass, BPF-5
   ships the ignore file with that one entry and acceptance switches
   to the default invocation.)
6. The CI step from 7.1 is wired into `.github/workflows/ci.yml`.
7. `CONTRIBUTING.md` gets a short subsection ("EnvGuard CI lint")
   explaining what the lint catches and how to silence a false
   positive via the ignore file. The subsection cites this design
   document and the EOL plan in section 9.

## 9. EOL plan

This lint is removed by BPF-9 (or whichever sub-task obsoletes
`EnvGuard` as the cap-test serialisation mechanism). To make the
deletion mechanical:

1. The script begins with the header:

   ```sh
   #!/usr/bin/env bash
   # DELETE ME WHEN BPF-9 LANDS.
   #
   # This lint exists because BufferPool capacity tests share a global
   # OnceLock singleton and must serialise env mutations via EnvGuard.
   # BPF-9 replaces the singleton with a per-test factory; once it
   # merges, this script, its CI step, the ignore file, the
   # CONTRIBUTING entry, and this design document are all deleted.
   #
   # Tracking: https://github.com/.../issues/2828
   ```

2. BPF-9 ships a single PR that removes:

   - `tools/ci/check_envguard.sh`
   - `tools/ci/envguard_lint.ignore`
   - The `EnvGuard lint` step in `.github/workflows/ci.yml`
   - The `EnvGuard CI lint` subsection in `CONTRIBUTING.md`
   - `docs/design/bpf-4-envguard-ci-lint-spec.md` (this file)

3. That PR auto-closes BPF-4 (#2822) and BPF-5 (#2823) via
   `Closes #2822, #2823` in the body.

## 10. Cross-links

- Memory: `[[project_bufferpool_test_serialization_fragile]]`
- Memory: `[[project_bufferpool_count_cap]]`
- Audit: `docs/audit/bufferpool-cap-tests-inventory.md` (BPF-1 / #2819)
- Audit: `docs/audit/bufferpool-envguard-gap-list.md` (BPF-2 / #2820)
- Source: `crates/platform/src/env.rs` (canonical `EnvGuard`)
- Source: `crates/engine/src/local_copy/buffer_pool/global.rs`
  (singleton, `ENV_BUFFER_POOL_SIZE`)
- Source: `crates/engine/src/local_copy/buffer_pool/pool.rs`
  (cap-touching builder methods)
- Config: `.config/nextest.toml` (`global-pool-serial` test-group;
  the runtime serialisation backstop the lint complements)
- Workflow: `.github/workflows/ci.yml` (`lint` job)
