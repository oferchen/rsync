# SZC.h - `--zero-copy` CLI semantics update after SEND_ZC promotion

Date: 2026-06-01
Scope: redefine `--zero-copy` / `--no-zero-copy` flag semantics once
SZC.g promotes `iouring-send-zc` to the default feature set.
Status: design proposal - pending SZC.g completion.
Predecessors:
- IUS-4 (PR #4661): original keep-opt-in decision.
- SZC.f (`send-zc-decision-revisit.md`): evidence-driven promote framework.
- SZC.g (pending): Cargo.toml feature default change - `iouring-send-zc`
  moves into the default feature set.

## 1. Problem statement

Today, `--zero-copy` advertises SEND_ZC dispatch on the io_uring
socket-send path. However, the `iouring-send-zc` cargo feature is not in
the default feature set. Default builds compile without the SEND_ZC code
path entirely, so `--zero-copy` silently downgrades to plain
`IORING_OP_SEND` on the socket path (while still enabling `sendfile`,
`splice`, and `copy_file_range` on other paths).

After SZC.g promotes `iouring-send-zc` to the default feature set:

1. SEND_ZC dispatch is always compiled in.
2. The runtime probe (`send_zc::is_supported`) gates dispatch on Linux
   6.0+ automatically under `ZeroCopyPolicy::Auto` (the default).
3. `--zero-copy` as currently defined sets `ZeroCopyPolicy::Enabled`,
   which forces SEND_ZC and errors on unsupported kernels. This becomes
   a footgun on kernels < 6.0 where the user might have intended only
   `sendfile`/`splice` zero-copy, not SEND_ZC specifically.

The flag semantics must be updated to reflect the new reality where
SEND_ZC is always available and auto-dispatched.

## 2. Current semantics (pre-SZC.g)

### CLI flags

| Flag | Policy set | Behavior on default build | Behavior with `--features iouring-send-zc` |
|------|-----------|--------------------------|------------------------------------------|
| (none) | `Auto` | sendfile/splice/copy_file_range auto; SEND_ZC unavailable | sendfile/splice/copy_file_range auto; SEND_ZC auto on 6.0+ |
| `--zero-copy` | `Enabled` | Forces sendfile/splice/cfr; SEND_ZC still unavailable | Forces sendfile/splice/cfr; forces SEND_ZC (error on < 6.0) |
| `--no-zero-copy` | `Disabled` | All zero-copy paths disabled; userspace read/write loops | All zero-copy paths disabled; userspace read/write loops |

### CLI help text (current)

```
--zero-copy    Allow I/O-level zero-copy syscalls (sendfile, splice,
               copy_file_range) when supported by the kernel. The io_uring
               SEND_ZC dispatch is gated behind the `iouring-send-zc` cargo
               feature, which is not in the default feature set; default
               builds downgrade to plain io_uring SEND on the socket path.
               Default policy is auto/enabled. Independent of filesystem-level
               reflink/CoW cloning.

--no-zero-copy Disable I/O-level zero-copy (policy=disabled); route through
               portable userspace read/write loops and force the platform copy
               fallback. Does not affect filesystem-level reflink/CoW cloning.
```

### Parser behavior (`crates/cli/src/frontend/arguments/parser/mod.rs`)

```rust
let zero_copy_policy = if matches.get_flag("zero-copy") {
    fast_io::ZeroCopyPolicy::Enabled
} else if matches.get_flag("no-zero-copy") {
    fast_io::ZeroCopyPolicy::Disabled
} else {
    fast_io::ZeroCopyPolicy::Auto
};
```

## 3. New semantics (post-SZC.g)

After promotion, the default policy (`Auto`) already activates all
zero-copy paths including SEND_ZC on 6.0+. The flag must be redefined.

## 4. Options analysis

### Option A - Deprecate `--zero-copy` (it is now always-on)

- `--zero-copy` emits a deprecation warning: "zero-copy I/O is now
  enabled by default; --zero-copy is a no-op and will be removed in a
  future release."
- Policy remains `Auto` (same as omitting the flag).
- `--no-zero-copy` retains its current meaning (`Disabled`).
- Removal of `--zero-copy` in a future major version.

**Pros:**
- Simplest mental model: zero-copy is an implementation detail, not a
  user-facing toggle.
- No confusion about what "force" means across kernel versions.
- Scripts with `--zero-copy` keep working (just warn, no error).

**Cons:**
- Users lose the ability to force SEND_ZC on < 6.0 for testing/bench.
- Deprecation warning noise in automated scripts.

### Option B - Repurpose `--zero-copy` as "force SEND_ZC even on < 6.0"

- `--zero-copy` sets `ZeroCopyPolicy::Enabled`, which errors if SEND_ZC
  probe returns false (kernel < 6.0 or missing IORING_OP_SEND_ZC).
- Useful for CI pipelines that want to verify they are running the fast
  path, failing loudly rather than silently falling back.
- `--no-zero-copy` retains its current meaning (`Disabled`).

**Pros:**
- Preserves the "assert this path is active" use case.
- No behavioral change for users already passing `--zero-copy` on 6.0+.

**Cons:**
- Breaks users who pass `--zero-copy` on 5.15 LTS expecting
  sendfile/splice zero-copy (which is available on 5.15). They now get
  a hard error because `Enabled` demands SEND_ZC specifically.
- Confusing: the flag name says "zero-copy" but the failure is about a
  specific io_uring opcode, not zero-copy in general.
- Risky: forcing SEND_ZC on early 6.0.x kernels with known fixes in
  6.0.5+ could cause data corruption or hangs.

### Option C - Keep `--zero-copy` as no-op with deprecation warning; add `--no-zero-copy` to disable

- `--zero-copy` emits deprecation warning, policy remains `Auto`.
- `--no-zero-copy` remains `Disabled` (all zero-copy paths off).
- Add `--send-zc` / `--no-send-zc` for fine-grained SEND_ZC control if
  the "force/disable just SEND_ZC" use case warrants a dedicated toggle.

**Pros:**
- Backward-compatible: existing scripts never break.
- Clean separation: `--send-zc` controls SEND_ZC specifically (power
  users, bench harnesses); `--zero-copy` was the coarse toggle.
- Migration path: deprecate `--zero-copy` display, keep parsing forever.

**Cons:**
- Proliferates flags. Two pairs (`--zero-copy`/`--no-zero-copy` and
  `--send-zc`/`--no-send-zc`) for overlapping concerns.
- `--send-zc` is implementation-detail-level, not user-facing.
- Ongoing maintenance of the deprecation path.

## 5. Recommendation

**Option A (deprecate `--zero-copy`)** with an escape hatch via
environment variable for the force-SEND_ZC use case.

Rationale:

1. **Zero-copy is not a user choice.** After SZC.g, zero-copy is the
   default I/O strategy - the system auto-selects the best available
   mechanism per kernel. Users should not need to opt in to something
   that is already on. The only meaningful user action is opting *out*
   via `--no-zero-copy` (for diagnostics, benchmarking, or
   compatibility).

2. **The "force" use case is niche.** Only bench harnesses and CI
   validation need to assert that SEND_ZC is specifically active. This
   is better served by `OC_RSYNC_REQUIRE_SEND_ZC=1` (env var) than a
   user-facing CLI flag. Environment variables are the standard channel
   for developer-facing runtime assertions.

3. **Backward compatibility.** Scripts passing `--zero-copy` continue to
   work unchanged - the flag is accepted, a warning is emitted to
   stderr, and behavior is identical to omitting it. No breakage.

4. **Minimal surface.** No new flags to document or maintain. The
   existing pair stays parseable (deprecation) while `--no-zero-copy`
   retains full functionality.

### Implementation plan

| Step | Change | File |
|------|--------|------|
| 1 | Parser: `--zero-copy` maps to `Auto` + emits deprecation warning | `crates/cli/src/frontend/arguments/parser/mod.rs` |
| 2 | Remove `Enabled` variant usage from the zero-copy code path (no caller sets it) | `crates/fast_io/src/policy.rs`, dispatch sites |
| 3 | Update CLI help text | `crates/cli/src/frontend/command_builder/sections/transfer_behavior_options.rs` |
| 4 | Add env var `OC_RSYNC_REQUIRE_SEND_ZC=1` for force-assert semantics | `crates/fast_io/src/io_uring/send_zc.rs` |
| 5 | Update man page | man page source |
| 6 | Release notes entry | changelog |

## 6. Man page text update

### Before (current)

```
--zero-copy
    Allow I/O-level zero-copy syscalls (sendfile, splice, copy_file_range)
    when supported by the kernel. The io_uring SEND_ZC dispatch requires
    building with --features iouring-send-zc; default builds use plain
    io_uring SEND on the socket path. Policy is auto/enabled by default.
    Independent of filesystem-level reflink/CoW cloning (--cow/--no-cow).

--no-zero-copy
    Disable I/O-level zero-copy; route all data through portable userspace
    read/write loops. Useful for benchmarking or diagnosing issues with
    kernel zero-copy paths. Does not affect CoW cloning.
```

### After (post-SZC.g)

```
--zero-copy
    Deprecated. Zero-copy I/O (sendfile, splice, copy_file_range, io_uring
    SEND_ZC) is now enabled by default and auto-detected per kernel version.
    This flag is accepted for backward compatibility but has no effect.
    Use --no-zero-copy to disable zero-copy paths.

--no-zero-copy
    Disable all I/O-level zero-copy paths; route data through portable
    userspace read/write loops. Useful for benchmarking, diagnosing kernel
    issues, or environments where zero-copy syscalls are restricted by
    seccomp policy. Does not affect CoW cloning (--cow/--no-cow).
```

## 7. CLI help text update

### Before

```
--zero-copy       Allow I/O-level zero-copy syscalls (sendfile, splice,
                  copy_file_range) when supported by the kernel. The io_uring
                  SEND_ZC dispatch is gated behind the `iouring-send-zc` cargo
                  feature, which is not in the default feature set; default
                  builds downgrade to plain io_uring SEND on the socket path.
                  Default policy is auto/enabled. Independent of
                  filesystem-level reflink/CoW cloning.

--no-zero-copy    Disable I/O-level zero-copy (policy=disabled); route through
                  portable userspace read/write loops and force the platform
                  copy fallback. Does not affect filesystem-level reflink/CoW
                  cloning.
```

### After

```
--zero-copy       [Deprecated] Zero-copy I/O is enabled by default. This flag
                  is accepted for backward compatibility but has no effect.
                  Use --no-zero-copy to disable.

--no-zero-copy    Disable all I/O-level zero-copy (sendfile, splice,
                  copy_file_range, io_uring SEND_ZC); route through portable
                  userspace read/write loops. Useful for benchmarking or
                  diagnostics. Does not affect CoW cloning (--cow/--no-cow).
```

## 8. Backward compatibility

### Users passing `--zero-copy` in scripts

- **Before SZC.g:** Flag sets `Enabled` policy. On default builds, this
  forces sendfile/splice/cfr but SEND_ZC is unavailable (compiled out).
  On feature builds, forces SEND_ZC (errors on < 6.0).
- **After SZC.g:** Flag is accepted, emits a one-time deprecation
  warning to stderr, and maps to `Auto`. Behavior is functionally
  identical to omitting the flag because `Auto` already activates all
  zero-copy paths. No breakage.

### Users passing `--no-zero-copy` in scripts

- **No change.** The flag continues to set `Disabled` policy. All
  zero-copy paths are bypassed.

### Users with `--features iouring-send-zc` in build scripts

- **After SZC.g:** The feature is in the default set. Specifying it
  explicitly is harmless (Cargo deduplicates). No action needed.

### Deprecation warning format

```
oc-rsync: WARNING: --zero-copy is deprecated (zero-copy I/O is now
default). The flag will be removed in a future release.
```

Emitted once per invocation, to stderr only, does not affect exit code.

## 9. Deprecation timeline

| Version | Behavior |
|---------|----------|
| v0.7.0 (SZC.g ships) | `--zero-copy` accepted with deprecation warning |
| v0.8.0 | `--zero-copy` hidden from `--help` output (still parsed) |
| v1.0.0 | `--zero-copy` removed; unrecognized flag error |

The timeline is conservative. The flag remains parseable through the
entire 0.x series to avoid breaking automated deployments.

## 10. Environment variable for force-assert

For bench harnesses and CI that need to verify SEND_ZC is active:

```
OC_RSYNC_REQUIRE_SEND_ZC=1 oc-rsync ...
```

Behavior:
- If `send_zc::is_supported()` returns true: proceed normally.
- If `send_zc::is_supported()` returns false: exit with error code 2 and
  message "SEND_ZC required but not supported on this kernel (need 6.0+)".

This keeps the "assert fast path" use case available without polluting
the user-facing CLI flag surface.

## 11. Interaction with `--no-zero-copy`

When both `--zero-copy` (deprecated) and `--no-zero-copy` appear, the
last one wins (existing `overrides_with` behavior in clap). After
deprecation, `--zero-copy` maps to `Auto` and `--no-zero-copy` maps to
`Disabled`. The override semantics remain unchanged - `--no-zero-copy`
always wins if it appears last.

`OC_RSYNC_REQUIRE_SEND_ZC=1` combined with `--no-zero-copy` is
contradictory. The CLI flag wins (env var is advisory); the process
proceeds with zero-copy disabled and does not error. This matches the
principle that explicit CLI flags override environment variables.
