# Mutex implementation policy: parking_lot vs std::sync

Tracking issue: oc-rsync task #1781.

## Summary

This audit verifies that the oc-rsync workspace honours the project policy
"prefer `std` unless a well-supported external crate provides a substantial,
documented advantage" (Code Quality section of the project conventions) for
synchronisation
primitives. The conclusion is that no first-party code in the workspace uses
`parking_lot::Mutex`, `parking_lot::RwLock`, `parking_lot_core`, or `spin`
directly. There are no `use parking_lot` imports, no `parking_lot::` paths in
source, and no `parking_lot` declarations in any workspace `Cargo.toml`.
`parking_lot` and `spin` enter the dependency graph only transitively, through
three third-party crates (`dashmap`, `serial_test`, `lazy_static`). No
migration is required. One small documentation gap is noted as a follow-up
(see Recommendation 1).

This audit is read-only. No `Cargo.toml` or source file is modified.

## Methodology

1. Workspace-wide grep for `parking_lot::`, `parking_lot_core::`, `spin::`,
   `use parking_lot`, `SpinLock`, `spin_lock`, `spinlock`. Tool: the workspace
   Grep tool (ripgrep underneath).
2. Workspace-wide grep for `parking_lot` in every `Cargo.toml`.
3. `Cargo.lock` parsed as ground truth for transitive dependencies. Cargo was
   not invoked, in keeping with the read-only constraint and the project rule
   "never run cargo locally" (auto-memory `feedback_no_local_cargo`).
4. Cross-check of every `dashmap::DashMap`, `serial_test::serial`, and
   `lazy_static!` call site for direct vs transitive classification.

## Inventory

### Direct project source matches

`rg --no-heading parking_lot` (excluding `Cargo.lock`) returns zero hits in
any `crates/**/src/`, `tests/`, `benches/`, or top-level Rust file.

`rg --no-heading 'parking_lot|parking_lot_core' --glob '**/Cargo.toml'`
returns zero hits in:

- the workspace root `Cargo.toml`;
- every `crates/*/Cargo.toml`;
- every `xtask/**/Cargo.toml`.

`rg --no-heading 'spin::|SpinLock|spin_lock|spinlock'` returns zero hits in
the entire workspace, source and `Cargo.toml` alike.

### `Cargo.lock` matches

Five lines reference `parking_lot` or `parking_lot_core` in `Cargo.lock`:

- `Cargo.lock:821` `dashmap` lists `parking_lot_core` in its `dependencies`
  array.
- `Cargo.lock:2099` package definition `parking_lot` v0.12.5 (registry).
- `Cargo.lock:2105` `parking_lot` lists `parking_lot_core` in its
  `dependencies` array.
- `Cargo.lock:2109` package definition `parking_lot_core` v0.9.12 (registry).
- `Cargo.lock:2972` `serial_test` lists `parking_lot` in its `dependencies`
  array.

Two lines reference `spin`:

- `Cargo.lock:1656` `lazy_static` lists `spin` in its `dependencies` array.
- `Cargo.lock:3116` package definition `spin` v0.9.8 (registry).

### Reverse dependency walk (parsed from `Cargo.lock`)

Three third-party crates pull `parking_lot`/`parking_lot_core` into the
graph. Their entry points into the workspace are:

| Crate | Workspace dependent | Declaration |
|-------|---------------------|-------------|
| `dashmap` 6.1.0 | `daemon` (optional, behind `concurrent-sessions` feature) | `crates/daemon/Cargo.toml:22,44`; workspace alias at `Cargo.toml:194` |
| `serial_test` 3.4.0 | `cli` (`[dev-dependencies]`, test-only) | `crates/cli/Cargo.toml:76` |
| `lazy_static` 1.5.0 | `num-bigint-dig` (transitive) and `sharded-slab` (transitive via `tracing-subscriber`) | `Cargo.lock:1933, 3036` |

`spin` is reachable only via `lazy_static`; no workspace crate depends on
`lazy_static` directly (verified by `rg --no-heading lazy_static --glob
'**/Cargo.toml'` returning zero hits and `rg 'use lazy_static'` returning zero
hits in source).

## Direct vs transitive classification

For each `parking_lot` or `spin` entry-point crate above:

- **`dashmap`** -> classification (b) "direct workspace dependency". Declared
  in the workspace at `Cargo.toml:194` (workspace dependencies section,
  `dashmap = "6.1"`) and re-declared in `crates/daemon/Cargo.toml:44` as an
  optional dependency activated by the `concurrent-sessions` feature
  (`crates/daemon/Cargo.toml:22`). Used at runtime in two daemon files:
  `crates/daemon/src/daemon/session_registry.rs:13`
  (`use dashmap::DashMap;`) and
  `crates/daemon/src/daemon/connection_pool/pool.rs:11`
  (`use dashmap::DashMap;`). The `parking_lot_core` dependency is contained
  inside `dashmap`'s shard locks; oc-rsync code never touches `parking_lot`
  types. From oc-rsync's perspective this is a transitive `parking_lot_core`,
  but a direct `dashmap`.
- **`serial_test`** -> classification (b) "direct workspace dependency",
  test-only. Declared in `crates/cli/Cargo.toml:76` under
  `[dev-dependencies]`. Used as the `#[serial]` attribute macro in
  `crates/cli/tests/environment_variable_defaults.rs:7`. Does not link into
  any production binary (`bin`, `daemon`, `cli` library targets); appears in
  test binaries only.
- **`lazy_static`** -> classification (c) "transitive only". No workspace
  `Cargo.toml` declares it; no source file imports it. Pulled in by
  `num-bigint-dig` (RSA / cryptography) and `sharded-slab` (used by
  `tracing-subscriber`). `spin` is reachable only via this path.

There are zero classification (a) "direct project code" matches, because no
workspace source file uses `parking_lot::` types.

## Justification audit

Per the task brief, justification is required for direct and workspace-level
uses (classifications (a) and (b)). The two classification (b) entries are:

### `dashmap` (parking_lot_core via shard locks)

- Justification documented? **Yes, partial.** The workspace `Cargo.toml`
  comment at line 193 reads `# Concurrent hash map - for shared state across
  daemon sessions`. The `daemon` `Cargo.toml` line 22 documents the feature
  flag (`# Concurrent session tracking - enables efficient shared state for
  multi-session daemons`). Both call sites also explain the choice in module
  docs:
  - `crates/daemon/src/daemon/connection_pool/pool.rs:1-4` "Thread-safe
    connection pool with per-IP rate limiting. Uses `DashMap` for lock-free
    concurrent access, allowing multiple threads to query and update
    connection state without blocking."
  - `crates/daemon/src/daemon/session_registry.rs:1-5` "Concurrent session
    tracking for the daemon accept loop. Provides a thread-safe registry
    backed by `DashMap` for tracking active daemon sessions. Multiple threads
    can query and update session state without blocking the main accept
    loop."
- Benchmark? **No.** No benchmark file (`crates/daemon/benches/*`,
  `benches/*`) compares `dashmap` against `std::sync::RwLock<HashMap<_, _>>`
  for the daemon connection pool or session registry. The existing rationale
  is correctness/architectural ("lock-free concurrent access from the accept
  loop"), not a measured 10%+ improvement.
- Hot path? **Borderline.** The connection pool is touched once per
  accept/disconnect (cold) and on every per-IP rate-limit check (warm but
  not on the byte path). The session registry is touched at session
  start/end. None of this is on the bytes-per-second hot path for a
  transfer.
- Note: the project policy is about `std::sync::Mutex` /
  `std::sync::RwLock` versus `parking_lot::Mutex` /
  `parking_lot::RwLock`. `dashmap` is a sharded concurrent map, not a
  parking_lot wrapper; the `parking_lot_core` use is internal to the crate
  for shard primitives. Replacing `dashmap` with `std::sync::RwLock<HashMap>`
  is an architectural change, not a primitive swap. The policy does not
  forbid sharded maps.

### `serial_test` (parking_lot via dev-dep)

- Justification documented? **Implicit.** `serial_test` is the
  community-standard crate for serialising tests that share global state
  (`std::env`, `chdir`). There is no `std`-only equivalent for the
  `#[serial]` attribute. The `cli` crate uses it precisely once
  (`crates/cli/tests/environment_variable_defaults.rs:7`) to serialise a
  test that mutates environment variables, which fits the project's
  `EnvGuard` discipline (`crates/cli/src/frontend/arguments/env.rs`).
- Benchmark? **N/A.** Test-only dependency; runtime perf irrelevant.
- Hot path? **No.** Compiled into test binaries only.

## Findings

### F1. dashmap pulls `parking_lot_core`; rationale documented but not benchmarked (LOW)

- **Classification:** (b) direct workspace dependency. Transitive
  `parking_lot_core` only.
- **Evidence:** `Cargo.toml:194`, `crates/daemon/Cargo.toml:22,44`,
  `crates/daemon/src/daemon/session_registry.rs:13`,
  `crates/daemon/src/daemon/connection_pool/pool.rs:11`,
  `Cargo.lock:811-822`.
- **Impact:** No oc-rsync code calls `parking_lot::` directly; the
  `parking_lot_core` symbols live entirely inside the `dashmap` crate's
  shard locks. Replacing them would require replacing `dashmap` itself.
  Module docs explain the design intent (lock-free concurrent access from
  the accept loop), but no benchmark documents a 10%+ win over
  `std::sync::RwLock<HashMap<_, _>>`. The daemon connection pool is not on
  the per-byte transfer hot path.
- **Severity:** LOW. The dependency is transitive; the direct API
  (`dashmap::DashMap`) is a well-supported community standard for
  multi-reader/writer maps and the design rationale is documented at the
  call sites.
- **Recommended fix:** Either (a) add a one-line comment at
  `Cargo.toml:193` noting why `dashmap` is preferred over a single
  `RwLock<HashMap>` for the daemon (concurrent reads from accept-loop
  threads, no global writer lock), or (b) add a small Criterion benchmark
  under `crates/daemon/benches/` comparing the two for the realistic
  daemon workload (N IPs, mixed read/write). Either keeps the dependency
  while matching the policy's "documented advantage" clause. No code
  change is required by this audit.

### F2. serial_test pulls `parking_lot` in dev-dependencies (LOW)

- **Classification:** (b) direct workspace dev-dependency.
- **Evidence:** `crates/cli/Cargo.toml:76`,
  `crates/cli/tests/environment_variable_defaults.rs:7`,
  `Cargo.lock:2963-2975`.
- **Impact:** None on production binaries. `serial_test` is the standard
  attribute-macro crate for `#[serial]` test gating. No `std`-only
  equivalent exists.
- **Severity:** LOW.
- **Recommended fix:** None. Keep the dev-dependency. Optional: add a
  one-line comment near `crates/cli/Cargo.toml:76` clarifying that the
  crate is required for environment-variable test serialisation (no
  `std`-only equivalent for `#[serial]`). No code change is required by
  this audit.

### F3. lazy_static / spin reachable only via cryptography and tracing (LOW)

- **Classification:** (c) transitive only.
- **Evidence:** `Cargo.lock:1651-1657` (lazy_static -> spin),
  `Cargo.lock:1928-1939` (num-bigint-dig depends on lazy_static),
  `Cargo.lock:3030-3037` (sharded-slab depends on lazy_static),
  `Cargo.lock:3520-3538` (tracing-subscriber depends on sharded-slab),
  `Cargo.lock:3116-3119` (spin v0.9.8 leaf).
- **Impact:** None reachable from oc-rsync source. No `use lazy_static`
  or `use spin` exists in the workspace.
- **Severity:** LOW. These crates ride into the graph behind well-known
  third-party deps (`tracing-subscriber`, the SSH/RSA crypto stack). They
  cannot be removed without dropping those crates.
- **Recommended fix:** None.

### F4. No first-party `parking_lot::Mutex` / `parking_lot::RwLock` usage (informational)

- **Classification:** N/A.
- **Evidence:** Workspace-wide `rg 'parking_lot::|use parking_lot'` returns
  zero hits in any `.rs` file. Workspace-wide
  `rg 'parking_lot' --glob '**/Cargo.toml'` returns zero hits.
- **Impact:** Positive. The codebase already complies with the policy
  "prefer std for `Mutex` / `RwLock`". Twenty-two `std::sync::(Mutex|RwLock)`
  call sites across eighteen first-party files; one `tokio::sync::Mutex`
  use at `crates/rsync_io/src/ssh/embedded/connect.rs:1` (justified by the
  async `russh` API surface).
- **Severity:** N/A.
- **Recommended fix:** None. Captured here so a future audit can confirm
  the property still holds.

## Recommendation

Concrete TODOs, in priority order:

1. **(LOW, optional, doc-only)** Annotate `Cargo.toml:193` (or
   `crates/daemon/Cargo.toml:44`) with the architectural reason for choosing
   `dashmap` over `std::sync::RwLock<HashMap<_, _>>`: concurrent
   read-mostly access from the accept loop and per-IP rate-limit threads,
   no global writer serialisation. Alternatively, add a small Criterion
   bench under `crates/daemon/benches/` that confirms the >=10% policy
   threshold for the realistic daemon workload. This addresses Finding F1.
2. **(LOW, optional, doc-only)** Add a one-line `# test serialisation for
   env-var tests, no std equivalent for #[serial]` comment near
   `crates/cli/Cargo.toml:76` to record the rationale for `serial_test`.
   This addresses Finding F2.
3. **No migrations.** No source file uses `parking_lot::`-typed locks; no
   workspace `Cargo.toml` declares `parking_lot` directly; no `std` lock
   needs to be migrated to anything. The policy is already met.
4. **Keep transitive `parking_lot` / `parking_lot_core` / `spin`.** They
   come in via `dashmap`, `serial_test`, and `lazy_static`. Removing them
   would require dropping those crates, which is out of scope and not
   justified by this policy (the policy targets first-party `Mutex` /
   `RwLock` choice, not transitive shard-lock implementations inside
   well-established third-party crates).
5. **Future-proof.** Add a one-line entry to a CI grep guard (for
   example, an existing `tools/` script or `tools/enforce_limits.sh`)
   that fails the build if `use parking_lot` or `parking_lot =` ever
   lands in workspace source or any `Cargo.toml`. Optional, but it
   prevents drift. Out of scope for this read-only audit; tracked here
   as a future TODO.

## References

- Policy source: project conventions, "Code Quality" section ("prefer std
  unless a well-supported external crate provides a substantial, documented
  advantage").
- Workspace dependencies block: `Cargo.toml:180-209` (no `parking_lot`
  entry).
- Daemon `concurrent-sessions` feature: `crates/daemon/Cargo.toml:16-26`.
- Daemon `dashmap` call sites:
  `crates/daemon/src/daemon/session_registry.rs`,
  `crates/daemon/src/daemon/connection_pool/pool.rs`.
- Test-only `serial_test` use: `crates/cli/Cargo.toml:76`,
  `crates/cli/tests/environment_variable_defaults.rs:7`.
- `Cargo.lock` package entries: `parking_lot` 0.12.5 (line 2099),
  `parking_lot_core` 0.9.12 (line 2109), `spin` 0.9.8 (line 3116),
  `lazy_static` 1.5.0 (line 1651).
- Audit format reference: `docs/audits/splice-ssh-stdio.md`.
