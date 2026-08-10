# WIND-2: Windows device-file handling strategy (spec)

Status: SPEC. WIND-1 (`docs/design/wind-1-device-file-audit.md`) produced the
upstream + current-tree audit; this document picks the strategy and locks the
interface contract. WIND-3 implements it, WIND-4 ships the regression test,
WIND-5 documents the support matrix.

## Scope

Define how a native (non-Cygwin) Windows oc-rsync receiver must behave when
the incoming flist carries a device entry (block or character device, FIFO,
or socket) or when a Windows sender encounters such an entry on the source
side. "Device file" in this spec covers the four upstream classifications:

- `IS_DEVICE(mode)` block + character device (`S_IFBLK`, `S_IFCHR`).
- `IS_SPECIAL(mode)` FIFO + socket (`S_IFIFO`, `S_IFSOCK`).

DOS-reserved console aliases (`NUL`, `CON`, `PRN`, `AUX`, `LPT1`-`LPT9`,
`COM1`-`COM9`) are explicitly out of scope: they are path-level reserved
names, not device inodes carried in an rsync flist. Reparse-point edge
cases (`IO_REPARSE_TAG_AF_UNIX`, OneDrive placeholders, junctions) are
covered by WPC-8 / WIND-1 section "Cygwin specifics" and are not changed
by this spec.

## Upstream behaviour (rsync 3.4.1)

All citations are against `target/interop/upstream-src/rsync-3.4.1/`.

- Wire encoding (sender): `flist.c:904-925` (receive path symmetric to
  the `send_file_entry` write at `flist.c:621-625`) encodes rdev major and
  minor only when `preserve_devices && IS_DEVICE(mode)` or, at protocol
  < 31, `preserve_specials && IS_SPECIAL(mode)`. Protocol >= 31 drops rdev
  for specials. `file_length` is forced to zero for device entries
  (`flist.c:923`).
- `--copy-devices` carve-out: `flist.c:949-954` downgrades a stray device
  entry to `S_IFREG | (mode & ACCESSPERMS)` with `modtime = now`, the
  pre-release patch guard that we already mirror in oc-rsync's flist
  receiver.
- Receiver-side syscall: `syscall.c:163-211` is `do_mknod`. The fake-super
  arm (`am_root < 0`, `syscall.c:168-174`) creates a 0600 placeholder with
  `open(O_WRONLY|O_CREAT|O_TRUNC)`. The real arm uses `mkfifo` (FIFO),
  `bind`-into-`AF_UNIX` (socket on platforms without `MKNOD_CREATES_SOCKETS`),
  or `mknod` (general). Exit on failure is the caller's standard
  `RERR_FILEIO` / `RERR_PARTIAL` mapping.
- Sender-side classification: `flist.c:1450-1453` decides whether the sender
  serialises a source-tree device at all; without `preserve_devices` /
  `preserve_specials` the entry is silently dropped from the flist before
  it ever hits the wire.
- Cygwin does not patch any of the above. Newlib supplies the POSIX
  facade; upstream relies on the C library and never knows it is running
  on Windows (see WIND-1 "Cygwin specifics", lines 113-119).

## oc-rsync today

All citations are against this worktree.

- Receiver writer: `crates/metadata/src/special.rs:292-305` defines the
  `#[cfg(not(unix))]` arms of `create_fifo_inner` and
  `create_device_node_inner` as silent `Ok(())` no-ops. No diagnostic, no
  counter increment, no file written.
- Engine executor: `crates/engine/src/local_copy/executor/special/device.rs:212-222`
  has an explicit `#[cfg(not(unix))]` block that comments "we can't actually
  create a device node" and then calls `register_created_path` with
  `CreatedEntryKind::Device` and `apply_file_metadata_with_options`, both
  against an inode that does not exist.
- Pipeline: `crates/transfer/src/receiver/transfer/pipeline.rs:316`
  computes `is_device_target = self.config.write.write_devices &&
  file_entry.is_device()` and forwards it without any Windows guard.
- Windows metadata module: `crates/metadata/src/windows/mod.rs:1-25` has
  no device-specific entry point; only the reparse classifier
  (`reparse::classify_path`) is exported.
- Flist data shape: `crates/flist/src/batched_stat/types.rs:28-30` /
  `144-146` already carries `rdev_major` / `rdev_minor` on `StatResult`
  and `StatxResult`, so the metadata round-trips through to the writer.

Net effect: a Windows oc-rsync receiver accepts the upstream flist
unchanged, runs the generator decision tree, registers
`CreatedEntryKind::Device` as bookkeeping, applies metadata to a
non-existent inode, and reports success. The destination is missing the
device / FIFO / socket entry and the operator has no signal that anything
was skipped. This violates the CLAUDE.md "Fail loud" rule.

## Strategy: (a) skip-and-warn, with opt-in (c) fake-super placeholder

Three options were considered in WIND-1 (audit lines 158-204). Reviewed
here against the WIND-2 contract:

| Option | Description | Decision |
|---|---|---|
| (1) Native NTFS reparse-point emulation | Mirror Cygwin's `IO_REPARSE_TAG_AF_UNIX` and friends, write reparse data via `FSCTL_SET_REPARSE_POINT` with `SE_CREATE_SYMBOLIC_LINK_NAME`. | REJECTED. Vendor-locked tag namespace, privilege requirement, anti-virus / UAC failure modes, no round-trip parity with non-Cygwin Windows tools. Revisit when `fast_io::windows::reparse` grows write support beyond WPC-8. |
| (a) Skip with warning | Receiver detects device/special, emits a `[receiver]`-tagged warning, increments a typed skip counter, leaves the destination untouched, propagates `RERR_PARTIAL` exit. Wire side is unchanged. | SELECTED (default). |
| (b) Refuse to transfer | Treat any device/special in the flist as malformed input, abort the transfer with a hard error. | REJECTED. Cross-platform mixed-content trees are common (Linux home dirs synced to Windows backup targets, etc.). A hard abort regresses real-world use cases that today complete with a fake "success". Wire compatibility also requires us to consume the flist entry regardless. |
| (c) Strip / map to regular file | Create a 0-byte regular file at the destination with the entry's mode bits masked to `ACCESSPERMS`. | OPT-IN ONLY, under `--fake-super`. Behind a flag this is upstream's documented behaviour (`syscall.c:168-174`) and round-trips with Linux peers via `user.rsync.%stat` (`xattrs.c:get_stat_xattr` / `set_stat_xattr`). Without `--fake-super` the placeholder is misleading. |

Final picture:

- Default Windows receiver: option (a), skip + warn + count.
- `--fake-super` Windows receiver: option (c) restricted to the upstream
  format. Placeholder file with `user.rsync.%stat` ADS (NTFS Alternate
  Data Stream) via `crates/metadata/src/xattr_windows.rs` (already wired
  through WPC-3 for `--xattrs`). Format string is upstream's
  `"%o %d,%d %d:%d"` (mode, rdev-major, rdev-minor, uid, gid).
- Sender side: no behavioural change. The flist contains device / special
  entries whenever `preserve_devices` / `preserve_specials` is in effect,
  exactly as upstream. Strip is the wrong layer; it would break parity
  with a Linux receiver and break round-trips.

### Wire contract

Unchanged. The flist still carries rdev major / minor for devices (and
for specials at protocol < 31). `file_length` is still forced to zero
upstream and on our receive path. No new protocol bytes, no negotiation
flag, no capability string change. WIND-2 is a receiver-local policy
change.

### Exit-code mapping

A transfer that skipped at least one device or special entry exits with
`RERR_PARTIAL` (24, upstream's "some files vanished" / partial-transfer
slot). When `--fake-super` writes a placeholder successfully the entry
is not counted as skipped. A failure to write the placeholder (ADS
denied, ENOSPC) is mapped to `RERR_FILEIO` (11) just like upstream's
`do_mknod` failure.

### Warning text and rate-limit

One-shot per entry. The receiver records the destination path in a
`HashSet<PathBuf>` keyed by canonical path so a re-run does not re-emit.
Suggested wording (not normative; UTS-3 final form to be locked in
WIND-3):

```
skipping device entry "<path>": Windows targets cannot create device nodes [receiver]
skipping fifo entry "<path>": Windows targets cannot create FIFOs [receiver]
skipping socket entry "<path>": Windows targets cannot create sockets [receiver]
```

### Counter wiring

A new field on the per-transfer stats struct (currently
`crates/transfer/src/stats/...`). Suggested name `skipped_devices` with
sub-counters `skipped_block / skipped_char / skipped_fifo / skipped_sock`
so WIND-5's support matrix can publish the breakdown. The summary line
(`-vvv`) prints the four counters when non-zero, matching the existing
delete-stats reporting style.

### Bookkeeping

The current
`crates/engine/src/local_copy/executor/special/device.rs:218-228`
`register_created_path(... CreatedEntryKind::Device ...)` call is wrong
on Windows: nothing was created. WIND-3 must replace it with a
`register_skipped_path(... SkippedEntryKind::Device ...)` (or equivalent)
on the non-Unix arm, and must NOT call `apply_file_metadata_with_options`
against the missing inode.

## Cross-platform parity requirement

WIND-3 MUST NOT change the Unix arms of any of:

- `crates/metadata/src/special.rs:133-289` (`create_fifo_inner`,
  `create_device_node_inner`, both Linux and Apple variants).
- `crates/engine/src/local_copy/executor/special/device.rs:212-217`
  (the `#[cfg(unix)]` block that calls `create_device_node_with_fake_super`).
- `crates/transfer/src/receiver/transfer/pipeline.rs:316-326`'s
  `is_device_target` computation on Unix.

WIND-4 MUST include negative-control assertions that the Unix tests for
the above paths still pass unchanged. The intent is that WIND-3 adds a
Windows-specific arm; it does not refactor or "improve" adjacent Unix
code. CLAUDE.md Rule 3 (Surgical Changes) is binding here.

## Follow-up tasks

- WIND-3: implement the spec. Touches
  `crates/metadata/src/special.rs` (Windows arms),
  `crates/engine/src/local_copy/executor/special/device.rs`
  (Windows arm + bookkeeping), `crates/transfer/src/stats/`
  (new counter), `crates/transfer/src/receiver/transfer/pipeline.rs`
  (Windows-side guard + warning emission). Ships behind no feature flag;
  Windows behaviour change is unconditional.
- WIND-4: regression test. CI matrix runs the test on Windows
  (`windows-2022`) and on Linux + macOS as the negative control. Test
  inputs are the WIND-1 audit's "Test plan" section (lines 207-232).
  Specifically:
  1. Windows receiver / no `--fake-super`: warn, skip, exit 24, no
     destination inode.
  2. Windows receiver / `--fake-super`: 0-byte placeholder, ADS contents
     match upstream `"%o %d,%d %d:%d"` byte-for-byte.
  3. Reverse direction: Windows sender reading a placeholder produced by
     (2) re-emits original rdev to a Linux receiver running as root.
  4. Idempotency: a second run produces no new warnings, no ADS churn.
  5. Cross-platform negative control: Unix arms unchanged.
- WIND-5: support-matrix documentation. Update
  `docs/operations/windows-support-matrix.md` (file to be created if
  absent) and the user-facing `README.md` table to advertise:
  "Device / FIFO / socket entries are skipped with a warning on Windows
  receivers. `--fake-super` round-trips them through NTFS ADS metadata."

## Open questions deferred to WIND-3

- Whether the skip counter is plumbed through the existing
  `--itemize-changes` (`-i`) output as a new item flag, or only through
  the per-run summary. Upstream has no analogue; either is defensible.
- Whether `--ignore-missing-args` semantics should suppress the warning
  when the source-side entry vanished between flist and receive (race
  with concurrent unlink). Defer until WIND-3 has the warning emission
  scaffold up.
- The exact public name of the new `SkippedEntryKind` variant: WIND-3
  picks during implementation review.
