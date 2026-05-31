# SEND_ZC fallback path audit (SZP-2)

Audit of how oc-rsync handles `IORING_OP_SEND_ZC` unavailability - both
when compiled with the feature but the kernel lacks support, and when the
feature is not compiled at all.

## 1. Feature compiled, kernel < 6.0 or SEND_ZC unsupported at runtime

### Code path

The `iouring-send-zc` cargo feature gates the availability of
`ZeroCopySender` and the SEND_ZC dispatch threshold constant. When the
feature is compiled in, the socket writer resolves SEND_ZC eligibility
at construction:

```
crates/fast_io/src/io_uring/socket_writer.rs:95
    let send_zc_active = config.allow_send_zc() && send_zc::is_supported();
```

Two gates must both pass:

1. **Policy gate** - `IoUringConfig::allow_send_zc()` returns `true` only
   when `zero_copy_policy == ZeroCopyPolicy::Enabled` (i.e., the user
   passed `--zero-copy`). The default `Auto` policy yields `false`.
   Location: `crates/fast_io/src/io_uring_common.rs:183`.

2. **Kernel probe** - `send_zc::is_supported()` performs a one-shot
   `IORING_REGISTER_PROBE` on a throwaway ring and checks whether opcode
   44 (`IORING_OP_SEND_ZC`) is advertised. The result is cached in a
   process-wide `AtomicI8` (`SEND_ZC_SUPPORTED`). Location:
   `crates/fast_io/src/io_uring/send_zc.rs:78-88`.

### What happens when the kernel reports unsupported

- `is_supported()` returns `false`.
- `send_zc_active` is set to `false` at construction.
- The socket writer uses `IORING_OP_SEND` for all payloads regardless of
  size.
- **No log message is emitted.** No metric is recorded.
- The fallback is entirely silent.

### What happens on a runtime SEND_ZC failure mid-session

If the initial probe incorrectly returned `true` but a later
`try_send_zc` call fails with `ErrorKind::Unsupported`, the socket
writer disables SEND_ZC for the remainder of its lifetime:

```
crates/fast_io/src/io_uring/socket_writer.rs:137
    self.send_zc_active = false;
```

Again, **no log message** is emitted. The fallback to `IORING_OP_SEND`
is silent.

### What `ZeroCopySender::new()` does on unsupported kernels

The high-level `ZeroCopySender` constructor returns
`Err(Unsupported)` immediately when `is_supported()` returns `false`
(line 311). Callers must handle this error - but no diagnostic is
logged by the constructor itself.

## 2. Feature not compiled (`iouring-send-zc` off)

### Code path

When the feature is disabled (the default build), the crate falls
through to the stub module:

- `crates/fast_io/src/io_uring_stub/send_zc.rs` - `is_supported()`
  always returns `false`; `try_send_zc()` always returns `Unsupported`.
- `ZeroCopySender::new()` always returns `Unsupported`.

The socket writer's `allow_send_zc()` method:

```
crates/fast_io/src/io_uring_common.rs:183-184
    pub fn allow_send_zc(&self) -> bool {
        matches!(self.zero_copy_policy, crate::ZeroCopyPolicy::Enabled)
    }
```

Even if the user passes `--zero-copy`, this enables the policy, but the
socket writer construction path (`from_raw_fd`) will still call
`send_zc::is_supported()` which returns `false` from the stub.
Result: `send_zc_active = false`.

### What code path handles sends

All socket sends go through `IORING_OP_SEND` via the batched send path
in `crates/fast_io/src/io_uring/batching.rs` (function
`submit_send_batch`). This is the normal non-zero-copy io_uring send
path - still faster than `write(2)` due to SQE batching, but involves a
kernel-side copy from userspace pages into the socket buffer.

### User indicator of missing performance

**None.** There is no log message, no CLI hint, and no status output
that tells the user their build is missing the `iouring-send-zc` feature
and therefore cannot use SEND_ZC even on a 6.0+ kernel.

The `--io-uring-status` output (`io_uring_capability_matrix()` in
`crates/fast_io/src/status.rs:133`) does include the feature gate state:

```
  feature gates:
    iouring-send-zc:      off
```

However, this is only visible when the user explicitly requests
`--io-uring-status`. During normal operation there is no proactive
signal.

## 3. Current degradation signals inventory

### Log messages

| Location | Level | Message | Condition |
|---|---|---|---|
| `crates/fast_io/src/io_uring/config.rs:198` | `debug_log!(Io, 1, ...)` | io_uring availability reason (general) | Once on first `is_io_uring_available()` call |
| `crates/fast_io/src/kernel_version.rs:204` | `debug_log!(Io, 1, ...)` | io_uring restriction type | When restriction != None, via `log_io_uring_probe_result()` |

**Total SEND_ZC-specific log messages: 0**

There is no log message at any level (debug, info, warn) that
specifically mentions SEND_ZC availability, fallback, or degradation.

### CLI output

| Mechanism | What it shows |
|---|---|
| `--io-uring-status` | Feature gate `iouring-send-zc: on/off` |
| `--io-uring-status` | Supported ops count (but not per-opcode breakdown) |
| `--version` | io_uring compiled/available status (no SEND_ZC detail) |

### Metrics / counters

**None.** There are no counters for:
- SEND_ZC submissions attempted vs succeeded
- Bytes sent via SEND_ZC vs regular SEND
- SEND_ZC fallback events

### Diagnostic accessors

| Function | Location | Purpose |
|---|---|---|
| `send_zc::is_supported()` | `send_zc.rs:78` | Cached probe result |
| `IoUringSocketWriter::send_zc_active()` | `socket_writer.rs:111` | Per-writer active state |
| `ZeroCopySender::registered_buffers_active()` | `send_zc.rs:447` | Buffer registration state |

These are query-only APIs for tests and diagnostics. They are not
surfaced to the user at runtime.

## 4. Silent degradation gaps

| Gap ID | Scenario | What happens | User signal |
|---|---|---|---|
| SZP-2.1 | `--zero-copy` passed, kernel < 6.0 | Falls back to `IORING_OP_SEND` | None |
| SZP-2.2 | `--zero-copy` passed, `iouring-send-zc` not compiled | Falls back to `IORING_OP_SEND` | None |
| SZP-2.3 | SEND_ZC probe returns unsupported at socket writer construction | Writer uses SEND only | None |
| SZP-2.4 | SEND_ZC fails mid-session (race/container restriction) | Writer disables SEND_ZC silently | None |
| SZP-2.5 | Default build on kernel 6.0+ (feature off) | SEND_ZC never attempted | None (unless `--io-uring-status` queried) |

## 5. Recommendations

### 5.1 Add info-level log on SEND_ZC probe outcome

In `IoUringSocketWriter::from_raw_fd()`, after resolving `send_zc_active`,
emit an info-level diagnostic when the user requested `--zero-copy` but
SEND_ZC is not available:

```rust
// After line 95 in socket_writer.rs:
if config.allow_send_zc() && !send_zc_active {
    logging::debug_log!(
        Io, 1,
        "SEND_ZC: unavailable (kernel does not advertise IORING_OP_SEND_ZC); \
         socket sends will use IORING_OP_SEND"
    );
}
```

This covers gaps SZP-2.1 and SZP-2.3.

### 5.2 Add info-level log on mid-session SEND_ZC disable

In the `Err(())` branch at `socket_writer.rs:137`:

```rust
logging::debug_log!(
    Io, 1,
    "SEND_ZC: disabling for this writer after runtime Unsupported error; \
     falling back to IORING_OP_SEND"
);
```

This covers gap SZP-2.4.

### 5.3 `--io-uring-status` should include SEND_ZC kernel state

The `io_uring_capability_matrix()` function in `status.rs` should add a
row showing the runtime SEND_ZC probe result when the `iouring-send-zc`
feature is compiled in:

```
  send_zc opcode:       yes (kernel 6.1 >= 6.0 required)
```

or:

```
  send_zc opcode:       no (kernel 5.15 < 6.0 required)
```

This gives operators a single command to diagnose SEND_ZC state without
reading source code.

### 5.4 Warn when `--zero-copy` is passed but feature not compiled

At the CLI layer (in `drive/config.rs` or the server flag parser), when
`zero_copy_policy == Enabled` but `cfg!(not(feature = "iouring-send-zc"))`,
emit:

```
warning: --zero-copy has no effect on socket sends because the binary was
built without the iouring-send-zc feature (SEND_ZC unavailable)
```

This covers gap SZP-2.2.

### 5.5 Connection to IKV-F (io_uring silent fallback observability)

The `IoUringRestriction` enum and `detect_io_uring_restriction()` in
`crates/fast_io/src/status.rs` (IKV-F.2 work) provides the pattern for
surfacing silent fallback. SEND_ZC observability should follow the same
pattern:

- A `SendZcStatus` enum: `Available`, `FeatureDisabled`,
  `KernelUnsupported { major, minor }`, `PolicyDisabled`.
- A `detect_send_zc_status()` function cached process-wide.
- Integration into `io_uring_capability_matrix()`.
- A `debug_log` emission at startup alongside the existing io_uring
  probe log.

This aligns with the broader IKV-F goal of making every io_uring
capability tier observable at runtime without code inspection.

## 6. Summary

SEND_ZC fallback is **completely silent** in the current codebase. No
log messages, no metrics, and no CLI output (outside the opt-in
`--io-uring-status` feature-gate display) inform the user that SEND_ZC
is not in use. The code is correct - fallback to `IORING_OP_SEND` is
functionally safe - but operationally opaque. An operator running on
kernel 6.0+ with `--zero-copy` has no way to confirm SEND_ZC is
actually active without attaching a debugger or reading `strace` output.

The fix is straightforward: 4 log statements and 1 status-matrix
addition. No wire protocol changes, no new dependencies.
