# SpillPolicy public API + env-var surface

Tracking issue: oc-rsync task #2335. Branch: `docs/spill-policy-api-design`.
Subtasks: STN-2..STN-14 (see implementation plan in section 7).

## TL;DR

Today the only spill knob exposed to callers is
`ConcurrentDeltaConfig.spill_threshold_bytes: Option<u64>` plus an optional
`spill_dir: Option<PathBuf>`
(`crates/engine/src/concurrent_delta/config.rs:35,41`). All other behaviour
- reclaim policy, granularity, codec choice - is hard-wired inside
`SpillableReorderBuffer` (`crates/engine/src/concurrent_delta/spill.rs:218`,
`HOT_ZONE = 16` at line 68, length-prefixed binary codec at lines 135-161).
This design promotes those hard-wired choices to a single public `SpillPolicy`
struct with five knobs, three environment overrides, and two ops-friendly CLI
flags. Migration keeps the existing field as a deprecated forwarding shim for
exactly one release.

## 1. Public Rust API

Shipped module (PR #4360): `crates/engine/src/concurrent_delta/spill/policy.rs`.
Re-exported from `crates/engine/src/concurrent_delta/spill.rs` and the
parent `crates/engine/src/concurrent_delta/mod.rs`. The decomposition
context (which submodules currently exist under
`crates/engine/src/concurrent_delta/spill/`) lives in
[`docs/audits/spill-rs-decomposition-plan.md`](../audits/spill-rs-decomposition-plan.md#current-layout);
this submodule satisfies the SPL-5 row of that migration table.

```rust
use std::path::PathBuf;

/// User-facing tuning surface for the bounded-memory spill layer.
///
/// `SpillPolicy::default()` returns the historical behaviour: no spill,
/// everything in memory. To enable spill set `threshold_bytes = Some(N)`;
/// every other field has a sensible default.
#[derive(Debug, Clone, Default)]
pub struct SpillPolicy {
    /// Memory budget (bytes) before items spill to disk. `None` disables
    /// spill entirely - the consumer stays on the bare `ReorderBuffer`
    /// path. `Some(0)` is rejected at validation time.
    pub threshold_bytes: Option<u64>,

    /// Directory backing the spill file. `None` defers to
    /// `std::env::temp_dir()` via a spooled tempfile that lives in memory
    /// up to 1 MB before rolling over.
    pub dir: Option<PathBuf>,

    /// Decision on spilled items once memory pressure recedes.
    pub reclaim_mode: ReclaimMode,

    /// Spill chunking unit. Coarser is faster, finer is more granular.
    pub granularity: SpillGranularity,

    /// Codec applied to the on-disk payload.
    pub compression: SpillCompression,
}

/// Behaviour after spilled items are reloaded into memory for delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReclaimMode {
    /// Reloaded items stay in memory until consumed. Lowest disk I/O,
    /// highest peak RSS recovery latency. This is the default and matches
    /// the current `SpillableReorderBuffer` behaviour.
    #[default]
    KeepInMemory,

    /// If `memory_used > threshold` is still true after a reload, the
    /// just-loaded items are eligible for re-spill. Trades extra disk
    /// I/O for a tighter memory bound under sustained pressure.
    ReSpillIfPressureContinues,
}

/// Unit at which the spill layer serialises and reloads items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpillGranularity {
    /// Spill the entire over-threshold tail of the reorder buffer in a
    /// single batch. Fewer syscalls, higher amortised throughput. Default.
    #[default]
    WholeBatch,

    /// Spill one item at a time, oldest-eligible first. Smoother memory
    /// curve, more syscalls per spill event.
    PerItem,
}

/// On-disk payload codec for spilled items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpillCompression {
    /// Raw length-prefixed binary payload (the current format). Default.
    #[default]
    None,

    /// zstd-compressed payload. Level matches `zstd::compression_level`
    /// semantics: negative for fast modes, positive for higher ratio.
    Zstd { level: i32 },
}
```

Constructors and builders follow the existing `ConcurrentDeltaConfig` style
(`config.rs:48-68`): `SpillPolicy::off()`, `SpillPolicy::with_threshold(n)`,
fluent `.with_dir(...)`, `.with_reclaim(...)`, `.with_granularity(...)`,
`.with_compression(...)`.

## 2. Defaults

| Field | Default | Rationale |
|-------|---------|-----------|
| `threshold_bytes` | `None` | Preserves the v0.5.8 behaviour audit in `reorderbuffer-spill-to-tempfile.md`: realistic transfers stay under the 64 MB high-water mark, so opt-in keeps the receiver lean. |
| `dir` | `None` | Defers to `std::env::temp_dir()` via `tempfile::SpooledTempFile`; 1 MB inline before rollover means zero disk hits for typical bursts. |
| `reclaim_mode` | `KeepInMemory` | Matches `SpillableReorderBuffer::reload_one_at` semantics today (`spill.rs:218`). The re-spill mode is opt-in for adversarial workloads. |
| `granularity` | `WholeBatch` | Mirrors `spill_excess()` (`spill.rs` core loop): one trim per insert that crosses the threshold. Lower syscall pressure under steady-state load. |
| `compression` | `None` | Length-prefixed binary today is decode-fast and zero-copy compatible. Zstd is opt-in when the spill directory is on slow/SMR storage. |

## 3. Environment variables

Names align with the existing `OC_RSYNC_*` namespace seen in `branding`,
`fast_io`, and `core` (`OC_RSYNC_BRAND`, `OC_RSYNC_DISABLE_IOURING`,
`OC_RSYNC_ASYNC_SSH`, `OC_RSYNC_FORCE_NO_COMPRESS`).

| Variable | Maps to | Accepted values |
|----------|---------|-----------------|
| `OC_RSYNC_SPILL_THRESHOLD_BYTES` | `threshold_bytes` | Integer with optional `K`/`M`/`G` suffix (case-insensitive, base 1024). Empty string clears. `0` rejected. |
| `OC_RSYNC_SPILL_DIR` | `dir` | Absolute or relative path. Created on first spill via `fs::create_dir_all`. |
| `OC_RSYNC_SPILL_RECLAIM` | `reclaim_mode` | `keep` -> `KeepInMemory`; `re-spill` -> `ReSpillIfPressureContinues`. Case-insensitive. |
| `OC_RSYNC_SPILL_GRANULARITY` | `granularity` | `whole-batch` -> `WholeBatch`; `per-item` -> `PerItem`. |
| `OC_RSYNC_SPILL_COMPRESSION` | `compression` | `none` -> `None`; `zstd` -> `Zstd { level: 3 }` (zstd default); `zstd:LEVEL` -> `Zstd { level: LEVEL }`. |

Precedence (highest wins): CLI flag > env var > programmatic `SpillPolicy` >
`SpillPolicy::default()`. This mirrors the precedence used by
`OC_RSYNC_FORCE_NO_COMPRESS` in `crates/cli/src/frontend/execution/drive/options.rs:389`.

## 4. CLI flags (ops-friendly subset)

Only the two highest-value knobs surface as CLI flags. The remaining three
stay env-only to keep the CLI tabular and to avoid promoting niche tuning
into the help screen.

| Flag | Argument | Mapping |
|------|----------|---------|
| `--spill-dir PATH` | `OsString` path | Sets `SpillPolicy.dir`. Mirrors the existing `--temp-dir` flag wiring at `parser/mod.rs:522-524`. |
| `--spill-threshold-bytes N[K\|M\|G]` | size string | Sets `SpillPolicy.threshold_bytes`. Same suffix grammar as `OC_RSYNC_SPILL_THRESHOLD_BYTES`. |

Both flags slot into `parsed_args/mod.rs` alongside `temp_dir`. Neither
appears in upstream rsync's option grammar, so we do not need a short form.

## 5. Validation rules

Performed in `SpillPolicy::validate()` and invoked by every constructor that
crosses a trust boundary (env-var loader, CLI parser, daemon config reader):

1. `threshold_bytes`: if `Some(n)`, then `n > 0`. `Some(0)` returns
   `SpillPolicyError::ZeroThreshold`.
2. `dir`: if `Some(p)`, the path is writable - tested via the same probe
   pattern as `SpillableReorderBuffer::with_spill_dir` (`spill.rs:315-334`):
   a `fs::create_dir_all` followed by a tempfile `create`/`unlink`
   round-trip. Failure surfaces as `SpillPolicyError::DirUnwritable { path, source }`.
3. `compression`: if `Zstd { level }`, then `level` is in `[-22, 22]` (the
   range accepted by the `zstd` crate). Out-of-range returns
   `SpillPolicyError::InvalidZstdLevel(level)`.
4. `reclaim_mode` and `granularity`: enum variants are statically bounded;
   no runtime check needed.
5. Env-var parser errors (`malformed suffix`, `unknown enum string`) wrap
   into `SpillPolicyError::InvalidEnvVar { var, value }` with the offending
   value redacted of newlines.

Validation runs at config-construction time, never inside the hot reorder
loop.

## 6. Migration story

Existing field on `ConcurrentDeltaConfig`:

```rust
pub spill_threshold_bytes: Option<u64>,
pub spill_dir: Option<PathBuf>,
```

Migration plan, single release window:

1. Add `pub spill: SpillPolicy` to `ConcurrentDeltaConfig` and mark the two
   legacy fields `#[deprecated(since = "0.5.10", note = "use ConcurrentDeltaConfig::spill instead")]`.
2. In every constructor (`ConcurrentDeltaConfig::with_spill_threshold`,
   `with_spill_dir`, `Default`), populate both the legacy fields and the
   new `spill: SpillPolicy` so older callers continue to see consistent
   values when they read either side.
3. `DeltaConsumer::spawn_with_config` reads `spill` first; if it is at
   `SpillPolicy::default()` and the legacy `spill_threshold_bytes` is
   `Some`, it forwards the legacy value via
   `SpillPolicy::with_threshold(...).with_dir_opt(...)`. This keeps the
   one-shot path working without a behavioural change.
4. After one release the legacy fields are removed; the deprecation note
   tells callers exactly which type to switch to.

`SpillPolicy::default()` is byte-equivalent to today's
`ConcurrentDeltaConfig::default()` (no spill), so the migration is
behaviour-preserving for every caller that has not opted in.

## 7. Five-step implementation plan

| Step | Subtask | Deliverable |
|------|---------|-------------|
| 1 | STN-2 .. STN-4 | Introduce `SpillPolicy`, enums, `SpillPolicyError`, and unit tests in `crates/engine/src/concurrent_delta/spill_policy.rs`. Pure type definition + validation, no wiring. |
| 2 | STN-5 .. STN-7 | Add env-var loader (`SpillPolicy::from_env`) covering the five `OC_RSYNC_SPILL_*` vars, including the `K`/`M`/`G` suffix parser shared with `--bwlimit` style flags. Property-tested round-trip env -> policy -> env. |
| 3 | STN-8 .. STN-10 | Wire `SpillPolicy` into `ConcurrentDeltaConfig` with the deprecated forwarding shim from section 6. Update `DeltaConsumer::spawn_with_config` and `SpillableReorderBuffer::new_from_policy` constructor. |
| 4 | STN-11 .. STN-12 | Plumb `--spill-dir` / `--spill-threshold-bytes` into `crates/cli/src/frontend/arguments/parser/mod.rs` and the matching fields in `parsed_args/mod.rs`. CLI help text only mentions the two ops flags; env vars documented in `docs/design/cli-tunability-flags.md`. |
| 5 | STN-13 .. STN-14 | Implement `ReclaimMode::ReSpillIfPressureContinues`, `SpillGranularity::PerItem`, and `SpillCompression::Zstd` behind the new policy fields, with parity tests against the default code path and a synthetic-pressure bench under `reorderbuffer_memory`. |

Each step lands as an independent PR keyed to its subtask range. Steps 1-3
are wire-protocol-neutral and ship in one release; steps 4-5 follow once
the policy surface is stable.

## 8. Out of scope

- Per-file or per-module overrides. The policy is process-wide.
- Daemon config-file syntax. The daemon will read `SpillPolicy::from_env`
  on startup; full `oc-rsyncd.conf` integration is tracked separately.
- Wire-protocol exposure. Spill is a purely receiver-side concern; no
  capability flag or negotiated parameter is involved (see
  `reorderbuffer-spill-to-tempfile.md` section "Scope").
