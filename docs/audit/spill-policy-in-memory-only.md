# SRO-2: SpillPolicy InMemoryOnly Variant Availability

**Status:** Complete
**Date:** 2026-05-26
**Predecessor:** SRO-1 (PR #5051) - spill temp-file failure mode audit

## Summary

`SpillPolicy` does not have an explicit `InMemoryOnly` variant. However, the
**default** `SpillPolicy` already provides equivalent semantics: when
`threshold_bytes` is `None` the consumer uses the bare in-memory
`ReorderBuffer` and no spill layer is instantiated at all.

## Current SpillPolicy Design

`SpillPolicy` is a flat struct (not an enum) in
`crates/engine/src/concurrent_delta/spill/policy.rs`. Spilling is opt-in -
controlled by a single `Option<u64>` field:

```
pub struct SpillPolicy {
    pub threshold_bytes: Option<u64>,   // None = no spilling
    pub dir: Option<PathBuf>,
    pub reclaim_mode: ReclaimMode,
    pub granularity: SpillGranularity,
    pub compression: SpillCompression,
    pub reclaim: SpillReclaim,
    pub memory_pressure_bytes: Option<u64>,
}
```

The decision tree in `ReorderMode::from_config()` at
`crates/engine/src/concurrent_delta/consumer/spawn.rs:47-64`:

- `threshold_bytes = Some(n)` - `ReorderMode::Spillable` - constructs a
  `SpillableReorderBuffer`, which can spill to disk.
- `threshold_bytes = None` - `ReorderMode::Bare` - constructs a plain
  `ReorderBuffer`, which is purely in-memory and has no disk I/O path
  whatsoever.

## Existing "Disable Spill" Mechanisms

### 1. Default behavior (threshold_bytes = None)

`SpillPolicy::default()` and `SpillPolicy::off()` both return a policy with
`threshold_bytes: None`. This means:

- The consumer thread instantiates `ReorderBuffer` (bare in-memory ring).
- No `SpillableReorderBuffer` is created.
- No temp-file is opened. No disk I/O occurs.
- Memory grows without bound up to the ring capacity.

This is already "in-memory only" in the strictest sense - not merely "spill
disabled" but "spill layer never instantiated."

### 2. CLI: omit --spill-threshold-bytes

The parser rejects `--spill-threshold-bytes 0` with an error message
directing the user to omit the flag entirely. From
`crates/cli/src/frontend/arguments/parser/mod.rs:928-929`:

> `0` is rejected - callers that want to disable spilling should omit the flag.

### 3. Environment variables

The env var `OC_RSYNC_SPILL_THRESHOLD_BYTES` is applied only when present.
Absent or unset vars leave `threshold_bytes` at `None`. There is no env var
to explicitly disable spilling - it is disabled by the absence of the
threshold var.

### 4. ConcurrentDeltaConfig::off()

`ConcurrentDeltaConfig::off()` returns a config with
`SpillPolicy::default()` (no spilling), matching
`ConcurrentDeltaConfig::default()`.

## Is an InMemoryOnly Variant Needed?

**No.** The current design already achieves full in-memory-only semantics
without an explicit variant. The reasons:

### The default is already in-memory-only

`SpillPolicy::default()` and `SpillPolicy::off()` produce a policy where no
spillable buffer is ever constructed. The bare `ReorderBuffer` has zero disk
I/O paths.

### No spill layer to suppress

An `InMemoryOnly` variant would only add value if there were a scenario where
the spill layer is active and needs to be explicitly suppressed while keeping
its other configuration. But `threshold_bytes = None` already achieves this
completely - the `SpillableReorderBuffer` constructor is never reached.

### The fallback on spill failure already re-inserts to memory

When spilling is enabled but fails (ENOSPC, temp dir vanished, codec error),
items are re-inserted into the in-memory ring via `restore_taken()` or
`force_insert()`. This means a failed spill degrades to in-memory-only
behavior on a per-event basis - but this is a failure-recovery path, not a
policy choice.

### No backpressure or error-on-overflow mode

The current bare `ReorderBuffer` handles overflow by either:

- Returning `CapacityExceeded` error (normal insert)
- Expanding via `force_insert` (unbounded growth within the ring)

There is no "error instead of spilling" mode because the decision between
spilling and not-spilling is made at construction time, not at insert time.
An `InMemoryOnly` policy variant would need to pick one of these existing
overflow strategies, which are already available via the bare ring.

## Call Sites That Check Policy Before Spilling

1. **`ReorderMode::from_config()`**
   (`crates/engine/src/concurrent_delta/consumer/spawn.rs:47-64`) -
   The single decision point. Checks `threshold_bytes.is_some()` to select
   between `Bare` and `Spillable` modes.

2. **`SpillableReorderBuffer::insert()`**
   (`crates/engine/src/concurrent_delta/spill/buffer/insert.rs:36`) -
   Checks `memory_used > threshold` or RSS pressure. This path is only
   reachable when a `SpillableReorderBuffer` was constructed (i.e.,
   `threshold_bytes` was `Some`).

3. **`SpillPolicy::is_enabled()`**
   (`crates/engine/src/concurrent_delta/spill/policy.rs:218-220`) -
   Returns `threshold_bytes.is_some()`. Used in test assertions and config
   validation. Not used in the insert/spill hot path.

## If InMemoryOnly Were Added Anyway

For completeness, here is where it would go and what it would mean:

**Location:** Could be an additional constructor on `SpillPolicy`, e.g.:
```rust
pub fn in_memory_only() -> Self {
    Self::default()  // Identical to off()
}
```

**Behavior options:**

| Variant | When memory exceeds budget | Disk I/O | Status |
|---------|---------------------------|----------|--------|
| Current `off()`/`default()` | Unbounded in-memory growth | None | Already exists |
| Hypothetical `InMemoryOnly` with cap | Error (exit code 11) | None | Would need new ReorderBuffer variant |
| Hypothetical `InMemoryOnly` with backpressure | Block producer thread | None | Would need crossbeam bounded channel changes |

None of these are warranted by the current architecture. The first row
already works. The second and third would require changes to `ReorderBuffer`
itself, not to `SpillPolicy`.

## CLI/Config Surface

- **`--spill-dir PATH`** - override spill directory.
- **`--spill-threshold-bytes BYTES`** - enable spilling at the given budget.
- **`OC_RSYNC_SPILL_DIR`** - env var override for spill directory.
- **`OC_RSYNC_SPILL_THRESHOLD_BYTES`** - env var override for threshold.
- **`OC_RSYNC_SPILL_COMPRESSION`** - env var for payload codec (none/zstd).
- No `--no-spill` flag exists. Not needed - spilling is opt-in.

## Conclusion

`SpillPolicy` does not need an `InMemoryOnly` variant. The default
construction (`SpillPolicy::default()` / `SpillPolicy::off()`) already
provides strict in-memory-only semantics where no spill layer is
instantiated, no temp files are created, and no disk I/O occurs. The
opt-in design (set `threshold_bytes` to enable) makes an explicit
disable variant redundant.
