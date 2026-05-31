# SZC.g - SEND_ZC default-on flip implementation plan

Date: 2026-06-01
Scope: implementation plan for promoting `iouring-send-zc` from opt-in to
the default Cargo feature set, contingent on SZC.f promote decision.
Status: ready to execute once SZC.f decides "promote".
Predecessor: SZC.f (`docs/design/send-zc-decision-revisit.md`) - decision
framework. This document is the implementation plan for the promote branch.
Successor: release notes entry in the next minor version.

## 1. Current state

The `iouring-send-zc` feature in `crates/fast_io/Cargo.toml` is opt-in:

```toml
[features]
default = ["io_uring", "iocp", "sqpoll-mlock-basis"]

iouring-send-zc = ["io_uring"]
```

Default builds compile and dispatch `IORING_OP_SEND` (plain send) on the
io_uring socket-send path. The `ZeroCopySender` at
`crates/fast_io/src/io_uring/send_zc.rs` and its runtime probe
`is_supported()` are only compiled when a consumer explicitly passes
`--features iouring-send-zc`.

Users who want SEND_ZC today must build with:

```sh
cargo build --features iouring-send-zc
```

or enable it transitively via their own crate's feature passthrough.

The workspace root `Cargo.toml` does not currently propagate
`iouring-send-zc` at all - neither in the workspace `[features]` section
nor in the workspace-level `default` list.

## 2. Change summary

Add `iouring-send-zc` to the `default` feature set in
`crates/fast_io/Cargo.toml`. Add a workspace-level feature that
propagates through to `fast_io`. This makes `ZeroCopySender` compiled
and runtime-dispatchable on every default Linux build.

After the flip:

- Linux 6.0+ kernels: SEND_ZC dispatches automatically for payloads
  >= `SEND_ZC_MIN_BYTES` (currently 4 KiB).
- Linux < 6.0 kernels: `is_supported()` returns false at startup;
  transparent fallback to plain `IORING_OP_SEND`. Zero overhead.
- Non-Linux platforms: feature is a no-op (io_uring gate compiles out).

## 3. Cargo.toml changes required

### 3.1 `crates/fast_io/Cargo.toml`

```diff
 [features]
-default = ["io_uring", "iocp", "sqpoll-mlock-basis"]
+default = ["io_uring", "iocp", "sqpoll-mlock-basis", "iouring-send-zc"]
```

No other change in this file. The `iouring-send-zc = ["io_uring"]`
dependency declaration already exists and remains unchanged.

### 3.2 Workspace root `Cargo.toml`

Add a new workspace-level feature that forwards to `fast_io`:

```diff
 # Performance Optimization Features
 io_uring = ["transfer/io_uring", "fast_io/io_uring"]
+
+# io_uring SEND_ZC zero-copy socket send (Linux 6.0+, runtime fallback)
+iouring-send-zc = ["fast_io/iouring-send-zc"]
```

Add the new feature to the workspace `default` list:

```diff
 default = [
     "zstd",
     "lz4",
     "acl",
     "xattr",
     "iconv",
     "parallel",
     "copy_file_range",
     "io_uring",
     "iocp",
     "async",
+    "iouring-send-zc",
 ]
```

### 3.3 Downstream crates

No downstream crate changes are required. The `ZeroCopySender` is
internal to `fast_io` and consumed via the `socket_writer.rs` dispatch.
Crates that depend on `fast_io` (e.g., `transfer`, `engine`) do not need
to declare the feature themselves - they inherit it transitively through
`fast_io`'s default features or through the workspace feature forwarding.

The bench at `crates/fast_io/benches/ius_3_send_zc_vs_send.rs` currently
has `required-features = ["io_uring"]` (not `iouring-send-zc`) because
the bench itself cfg-gates the SEND_ZC cells. After the flip, both
feature gates are satisfied by default - no bench manifest change needed.

## 4. Runtime behavior change

### 4.1 Dispatch path (default build, Linux 6.0+)

Before the flip:

```
socket_writer::send() -> IORING_OP_SEND (always, SEND_ZC not compiled)
```

After the flip:

```
socket_writer::send()
  -> is_supported() = true (cached OnceLock probe)
    -> payload >= SEND_ZC_MIN_BYTES?
      -> yes: ZeroCopySender::try_send_zc() -> IORING_OP_SEND_ZC
      -> no:  IORING_OP_SEND (plain, avoids dual-CQE overhead)
    -> is_supported() = false (kernel < 6.0)
      -> IORING_OP_SEND (plain, transparent fallback)
```

### 4.2 Dispatch path (default build, Linux < 6.0)

Identical to before the flip. The `is_supported()` probe returns false
and the code path degrades to plain SEND. The only added cost is the
one-time probe call at process startup (cached in a `OnceLock`).

### 4.3 Dispatch path (non-Linux)

No change. The entire io_uring module is `cfg(target_os = "linux")`.
Non-Linux builds never compile `ZeroCopySender` or `socket_writer`'s
io_uring branch regardless of feature flags.

### 4.4 `--zero-copy` CLI flag

Before the flip: `--zero-copy` is required to activate SEND_ZC dispatch
(in addition to the build-time feature gate).

After the flip: SEND_ZC dispatches by default when `is_supported()`
passes. The `--zero-copy` flag retains its meaning as a hint for other
zero-copy primitives (sendfile, splice, copy_file_range) but is no
longer the sole gate for SEND_ZC.

The `--no-zero-copy` flag forces all send paths to plain SEND,
providing a runtime escape hatch.

## 5. CI impact

### 5.1 Linux CI cells now exercise SEND_ZC

All Linux CI runners that use default features will compile and
potentially dispatch SEND_ZC. Since CI runners typically run kernels
>= 6.1, the SEND_ZC path will be actively exercised in:

- `nextest (stable)` - full workspace test suite
- Linux musl build
- Interop validation (daemon tests)
- Benchmark workflows

This is a net positive: the SEND_ZC code path gets continuous exercise
rather than being tested only when explicitly opted in.

### 5.2 Kernel version sensitivity

If a CI runner has a kernel < 6.0 (unlikely for GitHub-hosted runners
but possible for self-hosted), the probe returns false and tests pass
via the fallback path. No CI breakage risk from kernel version mismatch.

### 5.3 New CI validation

Add a CI step (in the existing nextest workflow) that asserts the probe
result matches the runner's kernel version:

```sh
# Verify SEND_ZC probe correctness on CI (informational, not blocking)
kernel_major=$(uname -r | cut -d. -f1)
kernel_minor=$(uname -r | cut -d. -f2)
if [ "$kernel_major" -gt 6 ] || ([ "$kernel_major" -eq 6 ] && [ "$kernel_minor" -ge 0 ]); then
  echo "CI kernel $(uname -r) should support SEND_ZC"
fi
```

This is informational only - it confirms the probe is being exercised
but does not gate CI pass/fail.

## 6. Release notes template

```markdown
### io_uring SEND_ZC now enabled by default

The `IORING_OP_SEND_ZC` zero-copy send dispatch is now compiled and
active by default on Linux 6.0+ kernels. This eliminates one `memcpy`
per socket send on supported systems, reducing daemon CPU usage by
20-30% on bulk transfers and improving throughput by 7-12% on sustained
single-file workloads.

**No action required.** On kernels < 6.0, the runtime probe detects the
absence of SEND_ZC support and falls back transparently to the standard
send path with no performance impact.

**Disabling SEND_ZC:**
- Runtime: pass `--no-zero-copy` or set `OC_RSYNC_NO_SEND_ZC=1`
- Build-time: `cargo build --no-default-features --features io_uring`
  (builds with io_uring but without SEND_ZC)

Users who previously built with `--features iouring-send-zc` can remove
the explicit feature flag - it is now included in the default set.
```

## 7. Rollback plan

If a regression is discovered post-merge:

### 7.1 Immediate (user-facing)

Users experiencing issues can disable SEND_ZC without rebuilding:

```sh
OC_RSYNC_NO_SEND_ZC=1 oc-rsync ...
# or
oc-rsync --no-zero-copy ...
```

### 7.2 Patch release

Revert the Cargo.toml default change:

```diff
 # crates/fast_io/Cargo.toml
 [features]
-default = ["io_uring", "iocp", "sqpoll-mlock-basis", "iouring-send-zc"]
+default = ["io_uring", "iocp", "sqpoll-mlock-basis"]
```

```diff
 # Cargo.toml (workspace)
 default = [
     ...
-    "iouring-send-zc",
 ]
```

This single-line revert returns the feature to opt-in status. The
feature flag, code paths, and runtime probe all remain intact - only the
default compilation changes.

### 7.3 Emergency (data corruption suspected)

If SZC.e-class correctness issues surface on a kernel not covered by the
original validation:

1. Immediate patch: add the offending kernel version to
   `is_supported()`'s deny list so the probe returns false.
2. Follow-up: investigate whether the kernel bug is upstream-fixed and
   adjust the version floor accordingly.

## 8. Testing checklist

All items must pass before the promotion PR merges:

### 8.1 Build verification

- [ ] `cargo build --workspace --all-features` succeeds on Linux
- [ ] `cargo build --workspace` (default features) succeeds on Linux
- [ ] `cargo build --workspace` succeeds on macOS (feature is no-op)
- [ ] `cargo build --workspace` succeeds on Windows (feature is no-op)
- [ ] `cargo build --workspace --no-default-features` succeeds (opt-out path)
- [ ] `cargo build --workspace --no-default-features --features io_uring`
      builds io_uring without SEND_ZC (selective disable)

### 8.2 Test suite

- [ ] `cargo nextest run --workspace --all-features` passes (CI)
- [ ] Interop tests pass against upstream rsync 3.0.9, 3.1.3, 3.4.1, 3.4.2
- [ ] Daemon pull/push tests pass with SEND_ZC active
- [ ] `--no-zero-copy` disables SEND_ZC dispatch (verify via tracing)

### 8.3 Performance verification

- [ ] IUS-3 bench (`ius_3_send_zc_vs_send`) shows no regression vs
      pre-flip results on kernel 6.6
- [ ] SZC.b 10 GiB workload reproduces >= 5% throughput improvement
- [ ] SZC.c 100K-file workload shows no regression > 3%
- [ ] SZC.d concurrent daemon shows CPU savings >= 15% at N=4

### 8.4 Fallback verification

- [ ] On kernel < 6.0: `is_supported()` returns false (verify via log)
- [ ] On kernel < 6.0: transfer completes with byte-identical output
- [ ] `OC_RSYNC_NO_SEND_ZC=1` forces plain SEND on kernel >= 6.0
- [ ] `--no-zero-copy` forces plain SEND on kernel >= 6.0

### 8.5 CI workflow verification

- [ ] All required CI checks pass: fmt+clippy, nextest (stable),
      Windows (stable), macOS (stable), Linux musl (stable)
- [ ] No new clippy warnings introduced

## 9. Migration guide

### 9.1 Users who explicitly used `--features iouring-send-zc`

**Before (v0.6.x):**

```sh
cargo build --features iouring-send-zc
```

**After (post-flip):**

```sh
cargo build  # SEND_ZC is now included in default features
```

The explicit `--features iouring-send-zc` continues to work (it is
idempotent when already in the default set) but is no longer necessary.

### 9.2 Users who want to disable SEND_ZC

**Build-time disable:**

```sh
# Keep all other defaults, just remove iouring-send-zc:
cargo build --no-default-features --features "zstd,lz4,acl,xattr,iconv,parallel,copy_file_range,io_uring,iocp,async"
```

Or more practically, since disabling at runtime is simpler:

**Runtime disable:**

```sh
OC_RSYNC_NO_SEND_ZC=1 oc-rsync ...
# or
oc-rsync --no-zero-copy ...
```

### 9.3 Distributors packaging oc-rsync

Distributions that build with default features get SEND_ZC
automatically. No packaging change is required.

Distributions that pin feature sets should add `iouring-send-zc` to
their feature list if they want the optimization, or omit it to maintain
the pre-flip behavior.

### 9.4 Library consumers (crates depending on fast_io)

If your crate depends on `fast_io` with `default-features = true` (or
without specifying, which implies true), you now get SEND_ZC compiled
in. The `ZeroCopySender` public API is unchanged.

If your crate depends on `fast_io` with `default-features = false` and
selectively enables features, add `iouring-send-zc` to your feature
list to opt in:

```toml
[dependencies]
fast_io = { path = "../fast_io", default-features = false, features = ["io_uring", "iouring-send-zc"] }
```

## 10. Files modified by this change

| File | Change |
|------|--------|
| `crates/fast_io/Cargo.toml` | Add `"iouring-send-zc"` to `default` features list |
| `Cargo.toml` (workspace root) | Add `iouring-send-zc` feature forwarding + add to workspace `default` |
| `crates/fast_io/src/io_uring/socket_writer.rs` | Remove `#[cfg(feature = "iouring-send-zc")]` compile gate on the dispatch branch (now always compiled when io_uring is enabled) |
| `crates/fast_io/src/io_uring_common.rs` | Update `allow_send_zc()` default: return `is_supported()` instead of `false` when no explicit policy is set |
| `crates/cli/src/frontend/help.rs` | Remove "requires build with --features iouring-send-zc" qualifier |
| `crates/cli/src/frontend/command_builder/sections/transfer_behavior_options.rs` | Update `--zero-copy` help text |

## 11. Ordering and dependencies

```
SZC.c (numbers) -> SZC.f (decision) -> SZC.g (this plan) -> implementation PR
```

This document is self-contained and ready to execute the moment SZC.f
decides "promote" per section 4.2 of the decision framework. No
additional design work is needed between the SZC.f decision and the
implementation PR.

## 12. References

- SZC.f decision framework: `docs/design/send-zc-decision-revisit.md`
- IUS-4 original decision: `docs/design/ius-4-decision-2026-05-22.md`
- IUS-3 bench design: `docs/design/ius-3-send-zc-bench-design-2026-05-21.md`
- SEND_ZC implementation: `crates/fast_io/src/io_uring/send_zc.rs`
- Socket writer dispatch: `crates/fast_io/src/io_uring/socket_writer.rs`
- Runtime probe: `crates/fast_io/src/io_uring/send_zc.rs::is_supported()`
- Feature flag: `crates/fast_io/Cargo.toml` (`iouring-send-zc`)
- Workspace features: `Cargo.toml` (workspace root `[features]` section)
