# BPF-3: EnvGuard gap remediation spec

## 1. Scope

BPF-3 adds `EnvGuard` wrappers to every BufferPool test identified in the
BPF-2 gap list (`docs/audit/bufferpool-envguard-gap-list.md`) that touches
the global singleton or reads `OC_RSYNC_BUFFER_POOL_SIZE` without holding
an `EnvGuard`. The goal is to eliminate environment-coupling fragility so
that these tests are robust under nextest parallel execution regardless of
test-name-based filter alignment.

This is a stop-gap. The long-term fix is the per-test factory API in
BPF-6/BPF-8, which gives each test an isolated `BufferPool` instance and
removes the need for environment-variable serialisation entirely. BPF-3
only adds `EnvGuard` wrappers and one test rename - no factory migration,
no nextest config changes beyond what the rename requires.

Prior art in this series:

- BPF-1 (#2819) - inventoried every cap-touching test
  (`docs/audit/bufferpool-cap-tests-inventory.md`).
- BPF-2 (#2820) - classified BPF-1 results by `EnvGuard` coverage
  (`docs/audit/bufferpool-envguard-gap-list.md`).
- BPF-4 (#2822) - CI lint spec for `EnvGuard` coverage.
- BPF-6 (#2824) - per-test factory API design.
- BPF-8 (#2826) - factory implementation.

## 2. Problem

The `GLOBAL_BUFFER_POOL` static `OnceLock<Arc<BufferPool>>` is process-wide.
Its default configuration reads `OC_RSYNC_BUFFER_POOL_SIZE` from the
environment via `GlobalBufferPoolConfig::default()`. Under nextest parallel
execution, two classes of race exist:

1. **Singleton init race.** A test that calls `global_buffer_pool()` or
   `init_global_buffer_pool` can execute concurrently with an env-var test
   that mutates `OC_RSYNC_BUFFER_POOL_SIZE`. The lazy-init path reads the
   env var while the mutation is in-flight, producing a non-deterministic
   pool size.

2. **Env-var read race.** A test that calls `GlobalBufferPoolConfig::default()`
   without touching the singleton still reads the env var. If another test
   has set the env var (even under `EnvGuard`), the reading test sees the
   mutated value when scheduled in the gap between the `set_var` and the
   `EnvGuard` drop.

Today, serialisation relies on the nextest `global-pool-serial` test-group
with filter `test(global_pool) | test(env_var)`. This is rename-fragile:
any test whose name does not match the filter runs in parallel with
everything else. BPF-2 found one HIGH-risk test that misses the filter
and five MEDIUM-risk tests that are filter-covered but have no defence
against ambient env state.

## 3. Complete gap list

Derived from the BPF-2 gap list with one addition (`config_default_matches_hardware_parallelism`)
that BPF-1 flagged as env-reading without EnvGuard but BPF-2 omitted because
it does not touch the singleton. It is included here because it reads
`OC_RSYNC_BUFFER_POOL_SIZE` and is vulnerable to env-var read races.

### 3.1 HIGH risk - misses nextest filter

| # | File | Test function | What it does | Why it is HIGH |
|---|------|---------------|--------------|----------------|
| 1 | `crates/engine/src/local_copy/buffer_pool/global.rs:321` | `init_after_lazy_init_returns_err` | Forces lazy init of `GLOBAL_BUFFER_POOL` via `global_buffer_pool()`, then calls `init_global_buffer_pool` expecting `Err`. | Name contains neither `global_pool` nor `env_var`, so the `global-pool-serial` filter does not match. Runs in full parallel with env-var tests. If `GlobalBufferPoolConfig::default()` executes while an env-var test has set `OC_RSYNC_BUFFER_POOL_SIZE`, the lazy init produces a pool with the wrong size, and the test may pass or fail non-deterministically depending on whether the OnceLock was already initialised by a prior test. |

### 3.2 MEDIUM risk - filter-covered but rename-fragile

| # | File | Test function | What it does | Why it is MEDIUM |
|---|------|---------------|--------------|------------------|
| 2 | `crates/engine/src/local_copy/buffer_pool/global.rs:182` | `global_pool_returns_arc` | Calls `global_buffer_pool()`; triggers lazy init of singleton. Capacity comes from `GlobalBufferPoolConfig::default()` which reads `OC_RSYNC_BUFFER_POOL_SIZE`. | Covered by `test(global_pool)` filter today. Renaming the test breaks serialisation silently. No defence against ambient env state. |
| 3 | `crates/engine/src/local_copy/buffer_pool/global.rs:192` | `global_pool_returns_same_instance` | Calls `global_buffer_pool()` twice; asserts `Arc::ptr_eq`. Reads (and may init) singleton. | Same as above. |
| 4 | `crates/engine/src/local_copy/buffer_pool/global.rs:200` | `global_pool_is_thread_safe` | Spawns 8 threads calling `global_buffer_pool()` + `acquire_from`. Exercises singleton init under concurrency. | Same as above. |
| 5 | `crates/engine/src/local_copy/buffer_pool/global.rs:217` | `global_pool_buffers_are_reusable` | Calls `global_buffer_pool()`, acquires/releases a buffer; mutates pool free-list state. | Same as above. |
| 6 | `crates/transfer/tests/buffer_pool_cross_crate.rs:54` | `global_pool_accessible_cross_crate` | Calls `global_buffer_pool()` from the `transfer` crate; reads `buffer_size`, `max_buffers`. Triggers lazy init if not yet initialised. | Filter-covered (name matches `test(global_pool)`). Cross-crate location means the `EnvGuard` inline type from `global.rs` is not in scope. |

### 3.3 LOW risk - env-reading without singleton

| # | File | Test function | What it does | Why it is LOW |
|---|------|---------------|--------------|---------------|
| 7 | `crates/engine/src/local_copy/buffer_pool/global.rs:157` | `config_default_matches_hardware_parallelism` | Reads `GlobalBufferPoolConfig::default()`, which reads `OC_RSYNC_BUFFER_POOL_SIZE`. Does NOT touch the singleton. | Does not match the `test(global_pool) | test(env_var)` filter. Reads the env var, so if another test has set it, this test sees the mutated value and the assertion (`max_buffers == available_parallelism`) fails. Probability is low because the env-var tests run serialised, but any future env-var test outside the filter group would break this test. |

## 4. Per-test migration plan

Each entry below specifies the exact change. All changes are in test code
only - no production code is modified.

### 4.1 HIGH: rename `init_after_lazy_init_returns_err`

**File:** `crates/engine/src/local_copy/buffer_pool/global.rs`

**Change:** Rename the test function from `init_after_lazy_init_returns_err`
to `global_pool_init_after_lazy_init_returns_err`.

**Rationale:** The rename makes the test match the existing nextest filter
`test(global_pool)`, pulling it into the `global-pool-serial` group with
`max-threads = 1`. This is a single-line change (the `fn` name) with no
logic modification.

**EnvGuard:** Also add `EnvGuard::remove(ENV_BUFFER_POOL_SIZE)` at the top
of the test body, so the test is robust against ambient env state
regardless of filter alignment. The inline `EnvGuard` type is already in
scope in the same `mod tests` block.

```rust
#[test]
fn global_pool_init_after_lazy_init_returns_err() {
    let _guard = EnvGuard::remove(super::ENV_BUFFER_POOL_SIZE);
    // ... existing body unchanged ...
}
```

### 4.2 MEDIUM: add EnvGuard to four `global_pool_*` tests in `global.rs`

**File:** `crates/engine/src/local_copy/buffer_pool/global.rs`

For each of the four tests (`global_pool_returns_arc`,
`global_pool_returns_same_instance`, `global_pool_is_thread_safe`,
`global_pool_buffers_are_reusable`), add a single line at the top of the
test body:

```rust
let _guard = EnvGuard::remove(super::ENV_BUFFER_POOL_SIZE);
```

**Rationale:** `EnvGuard::remove` ensures `OC_RSYNC_BUFFER_POOL_SIZE` is
unset for the duration of the test, so `GlobalBufferPoolConfig::default()`
falls back to `available_parallelism()` regardless of what other tests
may have set. The `remove` variant (rather than `set` to a specific
value) preserves the test's existing assertion logic, which expects the
auto-detected default.

**Env var:** `OC_RSYNC_BUFFER_POOL_SIZE` (via `super::ENV_BUFFER_POOL_SIZE`).

**Why `remove` instead of `set`:** These tests assert properties of the
global pool (Arc identity, thread safety, buffer reuse) and do not care
about a specific `max_buffers` value. Removing the env var lets the pool
default to hardware parallelism, matching the pre-existing test
expectations. Setting to a specific value would require updating assertions.

### 4.3 MEDIUM: add EnvGuard to `global_pool_accessible_cross_crate`

**File:** `crates/transfer/tests/buffer_pool_cross_crate.rs`

**Problem:** The inline `EnvGuard` from `global.rs` is not in scope here -
it lives inside `engine`'s `#[cfg(test)] mod tests` and is not exported.

**Option A (preferred):** Inline a minimal `EnvGuard` in
`buffer_pool_cross_crate.rs`, identical to the one in `global.rs`. This
is consistent with how 12+ other test files in the workspace handle the
same problem (each has its own inline `EnvGuard`).

**Option B:** Import `platform::env::EnvGuard` from the `platform` crate.
This requires adding `platform` as a `dev-dependency` of `transfer`.
Cleaner long-term, but adds a cross-crate dependency for a pattern that
BPF-8 will obsolete.

**Decision:** Option A. The inline is 30 lines, matches the existing
codebase convention, and avoids adding a dependency that BPF-8 removes.

```rust
// Near the top of the file, inside a #[cfg(test)] block or at module level:
struct EnvGuard {
    key: String,
    original: Option<String>,
}

impl EnvGuard {
    #[allow(unsafe_code)]
    fn remove(key: &str) -> Self {
        let original = std::env::var(key).ok();
        unsafe { std::env::remove_var(key) };
        Self { key: key.to_string(), original }
    }
}

impl Drop for EnvGuard {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        match &self.original {
            Some(val) => unsafe { std::env::set_var(&self.key, val) },
            None => unsafe { std::env::remove_var(&self.key) },
        }
    }
}
```

Then at the top of `global_pool_accessible_cross_crate`:

```rust
#[test]
fn global_pool_accessible_cross_crate() {
    let _guard = EnvGuard::remove("OC_RSYNC_BUFFER_POOL_SIZE");
    // ... existing body unchanged ...
}
```

**Note:** The env var key is a string literal here because
`engine::local_copy::buffer_pool::global::ENV_BUFFER_POOL_SIZE` is
a private const. Using the literal is acceptable for a stop-gap that
BPF-8 replaces entirely.

### 4.4 LOW: add EnvGuard to `config_default_matches_hardware_parallelism`

**File:** `crates/engine/src/local_copy/buffer_pool/global.rs`

Add `EnvGuard::remove(super::ENV_BUFFER_POOL_SIZE)` at the top of the
test body. The `EnvGuard` type is already in scope.

```rust
#[test]
fn config_default_matches_hardware_parallelism() {
    let _guard = EnvGuard::remove(super::ENV_BUFFER_POOL_SIZE);
    let config = GlobalBufferPoolConfig::default();
    // ... existing assertions unchanged ...
}
```

**Rationale:** This test asserts `max_buffers == available_parallelism()`.
If `OC_RSYNC_BUFFER_POOL_SIZE` is set by another test, the assertion
fails. Removing the env var ensures the auto-detected fallback is used.

## 5. Risk assessment

### 5.1 Flake probability without EnvGuard

| Test | Risk | Flake mechanism | Probability per CI run |
|------|------|-----------------|----------------------|
| `init_after_lazy_init_returns_err` | HIGH | Runs in full parallel. If scheduled between an env-var test's `set_var` and its `EnvGuard` drop, the lazy init reads the mutated value. | ~5-15% on high-parallelism runners (16+ cores). The window is small (microseconds) but nextest's default parallelism is aggressive. |
| `global_pool_returns_arc` | MEDIUM | Filter-covered. Flakes only if the test is renamed. | ~0% today, 100% on rename. |
| `global_pool_returns_same_instance` | MEDIUM | Same as above. | ~0% today, 100% on rename. |
| `global_pool_is_thread_safe` | MEDIUM | Same as above. | ~0% today, 100% on rename. |
| `global_pool_buffers_are_reusable` | MEDIUM | Same as above. | ~0% today, 100% on rename. |
| `global_pool_accessible_cross_crate` | MEDIUM | Same as above. | ~0% today, 100% on rename. |
| `config_default_matches_hardware_parallelism` | LOW | Runs in full parallel. Flakes if an env-var test sets `OC_RSYNC_BUFFER_POOL_SIZE` while this test reads `GlobalBufferPoolConfig::default()`. | ~1-5%. The config read is fast, but the window overlaps with env-var test execution. |

### 5.2 Risk of the remediation itself

- **Rename risk (item 4.1):** Renaming `init_after_lazy_init_returns_err`
  to `global_pool_init_after_lazy_init_returns_err` changes the function
  name only. No callers reference this test by name outside of nextest
  filters, which match on substring. The rename is additive to the filter
  match set.

- **EnvGuard overhead:** `EnvGuard::remove` calls `std::env::var` (read)
  then `std::env::remove_var` (write). On drop, it calls `set_var` or
  `remove_var` to restore. Total: 2-3 syscalls per test, under 1 us.
  Negligible.

- **False sense of safety:** Adding `EnvGuard` to tests that already have
  filter-based serialisation is belt-and-suspenders. The env guard protects
  against future filter breakage; it does not replace the filter. Both
  mechanisms remain active until BPF-8 removes the singleton dependency.

## 6. Nextest filter update

The rename in section 4.1 does not require a nextest config change. The
existing filter `test(global_pool) | test(env_var)` uses substring matching.
Renaming `init_after_lazy_init_returns_err` to
`global_pool_init_after_lazy_init_returns_err` causes the test to match
`test(global_pool)`, pulling it into the `global-pool-serial` group
automatically.

No filter widening is needed. No new test-group is needed. The only
config change would be if we chose to widen the filter (e.g., adding
`| test(init_after_lazy_init)`) instead of renaming; we chose the rename
because it is simpler and self-documenting.

## 7. Testing and verification

### 7.1 Correctness

After applying all changes, run the affected tests individually:

```sh
cargo nextest run -p engine --all-features \
  -E 'test(global_pool) | test(env_var) | test(config_default)' \
  --color never
```

All tests must pass. The `global_pool_init_after_lazy_init_returns_err`
renamed test must appear in the output (confirming the rename took effect).

### 7.2 Serialisation verification

Confirm the renamed test is captured by the `global-pool-serial` group:

```sh
cargo nextest list -p engine --all-features \
  -E 'test(global_pool) | test(env_var)' \
  --color never 2>&1 | grep init_after_lazy_init
```

Expected output includes `global_pool_init_after_lazy_init_returns_err`.

### 7.3 Parallel stress test

Run the full buffer pool test suite N times under maximum parallelism to
confirm no flakes:

```sh
for i in $(seq 1 20); do
  cargo nextest run -p engine --all-features \
    -E 'test(/buffer_pool/) | test(global_pool) | test(env_var)' \
    --color never --no-fail-fast 2>&1 | tail -1
done
```

All 20 runs must report 0 failures.

### 7.4 Cross-crate test

```sh
cargo nextest run -p transfer --all-features \
  -E 'test(global_pool_accessible_cross_crate)' \
  --color never
```

Must pass with the new inline `EnvGuard`.

## 8. Relationship to BPF-6/BPF-8 (per-test factory)

BPF-3 is explicitly a short-term fix. The `EnvGuard` pattern has inherent
limitations:

1. **It serialises, not isolates.** Two `EnvGuard`-protected tests still
   share the same process environment. Serialisation (via nextest
   `max-threads = 1`) is the only way to prevent races. This caps
   parallelism for the affected tests.

2. **It is convention-enforced.** A new test author who forgets `EnvGuard`
   re-introduces the race. BPF-4/BPF-5 add a CI lint to catch this, but
   the lint is heuristic (text matching, not type-level).

3. **It couples tests to global state.** Every test that uses
   `global_buffer_pool()` implicitly depends on whether the `OnceLock`
   was already initialised by a previous test in the same process. This
   makes test outcomes order-dependent.

BPF-6 designs and BPF-8 implements a `BufferPool::isolated()` factory
that returns a self-contained pool instance owned by the test. Isolated
pools:

- Do not touch `GLOBAL_BUFFER_POOL` or any `OnceLock`.
- Do not read or write `OC_RSYNC_BUFFER_POOL_SIZE`.
- Can run under full nextest parallelism with no serialisation group.
- Eliminate `EnvGuard` as a test-authoring requirement for cap-tests.

Once BPF-9 migrates cap-tests to the factory, the `EnvGuard` wrappers
added by BPF-3 become dead code. BPF-10 removes them along with the
CI lint (BPF-4/BPF-5).

The five env-var contract tests (`env_var_overrides_pool_size`,
`env_var_zero_ignored`, `env_var_non_numeric_ignored`,
`env_var_negative_ignored`, `env_var_unset_uses_auto`) are NOT migrated
by BPF-9. They test the `GlobalBufferPoolConfig::default()` env-var
parsing path, which is production code that must remain exercised. These
tests keep `EnvGuard` permanently (or until the env-var config surface
itself is retired).

## 9. Change summary

| # | File | Change type | Lines touched |
|---|------|-------------|---------------|
| 1 | `crates/engine/src/local_copy/buffer_pool/global.rs` | Rename `init_after_lazy_init_returns_err` -> `global_pool_init_after_lazy_init_returns_err` | 1 |
| 2 | `crates/engine/src/local_copy/buffer_pool/global.rs` | Add `EnvGuard::remove` to `global_pool_init_after_lazy_init_returns_err` | 1 |
| 3 | `crates/engine/src/local_copy/buffer_pool/global.rs` | Add `EnvGuard::remove` to `global_pool_returns_arc` | 1 |
| 4 | `crates/engine/src/local_copy/buffer_pool/global.rs` | Add `EnvGuard::remove` to `global_pool_returns_same_instance` | 1 |
| 5 | `crates/engine/src/local_copy/buffer_pool/global.rs` | Add `EnvGuard::remove` to `global_pool_is_thread_safe` | 1 |
| 6 | `crates/engine/src/local_copy/buffer_pool/global.rs` | Add `EnvGuard::remove` to `global_pool_buffers_are_reusable` | 1 |
| 7 | `crates/engine/src/local_copy/buffer_pool/global.rs` | Add `EnvGuard::remove` to `config_default_matches_hardware_parallelism` | 1 |
| 8 | `crates/transfer/tests/buffer_pool_cross_crate.rs` | Add inline `EnvGuard` struct (~30 lines) | ~30 |
| 9 | `crates/transfer/tests/buffer_pool_cross_crate.rs` | Add `EnvGuard::remove` to `global_pool_accessible_cross_crate` | 1 |

**Total:** ~38 lines added, 0 lines removed (the rename replaces 1 line).

## 10. Acceptance criteria

1. `init_after_lazy_init_returns_err` is renamed to
   `global_pool_init_after_lazy_init_returns_err` and matches the
   `test(global_pool)` nextest filter.
2. All seven gap-list tests hold `EnvGuard::remove(ENV_BUFFER_POOL_SIZE)`
   (or the literal `"OC_RSYNC_BUFFER_POOL_SIZE"` in the cross-crate case)
   for the duration of their body.
3. `cargo nextest run -p engine --all-features -E 'test(global_pool) | test(env_var)'`
   passes with 0 failures.
4. `cargo nextest run -p transfer --all-features -E 'test(global_pool)'`
   passes with 0 failures.
5. The five existing env-var tests (`env_var_overrides_pool_size` etc.)
   are unchanged.
6. No production code is modified.
7. No nextest config changes are required (the rename is sufficient).

## 11. Cross-references

- BPF-1 inventory: `docs/audit/bufferpool-cap-tests-inventory.md`
- BPF-2 gap list: `docs/audit/bufferpool-envguard-gap-list.md`
- BPF-4 CI lint spec: `docs/design/bpf-4-envguard-ci-lint-spec.md`
- BPF-6 factory API design: `docs/design/bpf-6-buffer-pool-factory-api.md`
- BPF-8 factory implementation: `docs/design/bpf-8-buffer-pool-factory-impl.md`
- Inline EnvGuard: `crates/engine/src/local_copy/buffer_pool/global.rs:230-269`
- Canonical EnvGuard: `crates/platform/src/env.rs:19`
- Nextest config: `.config/nextest.toml` (lines 36-45, `global-pool-serial`)
- Global pool singleton: `crates/engine/src/local_copy/buffer_pool/global.rs:30`
- Env var const: `crates/engine/src/local_copy/buffer_pool/global.rs:65`
