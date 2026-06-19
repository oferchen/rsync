# WIND-1: Cygwin rsync device-file handling - Windows reference audit

Status: AUDIT + SPEC scaffold. WIND-2 produces the formal Windows strategy
spec, WIND-3 implements it, WIND-4 ships the regression test.

## Scope

Document how upstream rsync 3.4.4 handles device and special files (block
devices, character devices, FIFOs, sockets), inventory the gaps that exist
today when oc-rsync encounters such an entry on Windows, and recommend the
strategy for closing those gaps without breaking the cross-platform wire
contract.

## Upstream reference behaviour (rsync 3.4.4)

The upstream tarball is fetched into
`target/interop/upstream-src/rsync-3.4.4/`. Citations below use
`file:line-range` against that tree.

### Type classification macros

- `rsync.h:1310-1316` defines `S_ISBLK` and `S_ISCHR` as POSIX-style fallback
  macros so the tree compiles on systems whose `sys/stat.h` lacks them
  (Cygwin gets POSIX semantics here through newlib).
- The wire-level groupings `IS_DEVICE(mode)` and `IS_SPECIAL(mode)` (defined
  earlier in `rsync.h`) drive every other device-handling branch in the
  codebase and split the four kinds as:
  - device := block-device + char-device
  - special := FIFO + socket

### CLI surface and per-option globals

- `options.c:52-61` declares the in-process globals
  `copy_devices`, `write_devices`, `preserve_devices`, `preserve_specials`
  that gate the rest of the device pipeline.
- `options.c:672-678` wires the long options `--devices`, `--no-devices`,
  `--copy-devices`, `--write-devices`, `--specials`, `--no-specials` to
  those globals.
- `options.c:1560-1569` implements the `-D` / `--no-D` aggregate shortcuts
  (which `-a` expands to and which clears both `preserve_devices` and
  `preserve_specials`).

### Wire encoding (sender)

- `flist.c:447-474` computes the `XMIT_*_RDEV*` flags on a per-entry basis
  for device entries (`IS_DEVICE`) and for specials at protocol < 31
  (`IS_SPECIAL`). Protocol >= 31 stops sending rdev for specials.
- `flist.c:633-648` performs the wire write of the rdev major/minor (varint
  major, then varint or byte minor depending on protocol version).

### Wire encoding (receiver) and entry inflation

- `flist.c:932-953` is the symmetric receive path: it parses rdev major and
  minor, calls `MAKEDEV(major, minor)`, reserves `DEV_EXTRA_CNT * EXTRA_LEN`
  bytes of trailing storage when the kind is `IS_DEVICE`, and forces
  `file_length = 0`.
- `flist.c:977-985` is the `--copy-devices` carve-out: a device entry that
  the sender mistakenly serialized as a real file is downgraded to
  `S_IFREG | (mode & ACCESSPERMS)` with `modtime = now`, mirroring the
  protect-against-pre-release-patches comment in upstream.

### Generator: create-or-skip decision

- `generator.c:1627-1693` is the device/special arm of the generator: it
  decides whether to create a device node based on `am_root && preserve_devices`
  (devices) or `preserve_specials` (specials), runs the quick-check parity
  branch via `quick_check_ok` against `sx.st`, falls back through `try_dests_non`
  for `--copy-dest` / `--link-dest`, emits the itemize line, and ultimately
  calls `atomic_create`.
- `generator.c:2000-2053` is `atomic_create`: it computes a temp name,
  optionally calls `make_backup`, then dispatches to `do_symlink_at`,
  `hard_link_one`, or `do_mknod_at` depending on which of {slnk, hlnk, rdev}
  was provided.

### Receiver-side syscalls (`do_mknod` + `do_mknod_at`)

- `syscall.c:472-520` is `do_mknod`. The fake-super branch (`am_root < 0`)
  creates a regular empty file at mode 0600 instead of a real device node,
  storing the device metadata only in xattrs. Otherwise it picks between
  `mkfifo` (FIFO), `bind`-into-`AF_UNIX` (socket on platforms without
  MKNOD_CREATES_SOCKETS), and `mknod` (real device / FIFO).
- `syscall.c:536-615` is `do_mknod_at`, the symlink-race-safe variant: it
  opens the parent through `secure_relative_open`, then calls
  `mkfifoat`/`mknodat` against that dirfd. The fake-super arm in this
  variant uses `openat(dfd, O_NOFOLLOW | O_CREAT)` to plant a 0600
  placeholder under the secure parent. Sockets fall back to path-based
  `do_mknod` because `bindat(2)` is non-portable; this is documented as a
  residual.

### Backup, delete, and fake-super interactions

- `backup.c:197-205` short-circuits the hard-link fast path for devices and
  specials when `CAN_HARDLINK_SPECIAL` is undefined, forcing the copy code.
- `backup.c:278-281` falls through to `do_mknod_at(buf, mode, rdev)` when
  taking a backup of an existing device/special, so backup output preserves
  the type rather than degrading to a regular file.
- `delete.c:185-194` accounts deletions per type, incrementing
  `stats.deleted_devices` for `IS_DEVICE(mode)` and `stats.deleted_specials`
  otherwise. `delete.c:235-237` maps device kind to the `DEL_FOR_DEVICE`
  flag used by `get_del_for_flag`.
- `tls.c:67-105` is the standalone `tls` helper that round-trips the same
  `rsync.%stat`/`user.rsync.%stat` xattr layout, demonstrating the
  on-disk encoding rsync expects on systems whose VFS rejects mknod.
- `xattrs.c:1138-1226` is `get_stat_xattr`: under `--fake-super` (`am_root < 0`)
  it reads `user.rsync.%stat` formatted as `"%o %d,%d %d:%d"`
  (mode, rdev-major, rdev-minor, uid, gid) and rehydrates the synthetic
  stat back to a `S_IFBLK`/`S_IFCHR`/`S_IFIFO`/`S_IFSOCK` mode plus
  `MAKEDEV(major, minor)`. `set_stat_xattr` (immediately below in the same
  file) is the writer side that we would mirror.

### Cygwin specifics

Upstream rsync has no Cygwin-specific code path: every macro above resolves
through newlib, which exposes a POSIX surface backed by NTFS reparse points
tagged `IO_REPARSE_TAG_AF_UNIX` and `IO_REPARSE_TAG_CYGWIN_*` for FIFO and
socket emulation, and through plain regular files with magic content for
character/block devices. The portable handling is therefore "call `mknod` /
`stat` and trust the C library"; the actual Windows-visible artefact is an
NTFS reparse point or a magic regular file owned by the Cygwin install.

## oc-rsync today: gap inventory

All citations are against the current worktree.

- `crates/metadata/src/special.rs:292-305` - the non-Unix arms of
  `create_fifo_inner` and `create_device_node_inner` are silent
  `Ok(())` no-ops. The destination is left as whatever it was on entry
  (typically absent), and the generator believes the entry was created.
- `crates/engine/src/local_copy/executor/special/device.rs:212-222` - the
  `#[cfg(not(unix))]` arm is an explicit "Windows / non-Unix: we can't
  actually create a device node" comment block that bookkeeping still
  walks past, registering the path as `CreatedEntryKind::Device` even
  though no file was created. Mode/owner metadata is then applied to a
  non-existent inode.
- `crates/metadata/src/windows/mod.rs:1-25` - the Windows metadata module
  exports the reparse classifier (WPC-8') but has no device helper. There
  is no module-level analogue of `mknodat` and no plumbing into the
  reparse-tag space (`IO_REPARSE_TAG_AF_UNIX` etc.) that the Cygwin
  artefact uses on disk.
- `crates/transfer/src/receiver/transfer/pipeline.rs:316` - the receiver
  pipeline computes `is_device_target = self.config.write.write_devices &&
  file_entry.is_device()` and threads it into the response handler with
  no Windows fallback path; the special-file branch downstream then calls
  the no-op writers above.
- `crates/flist/src/batched_stat/types.rs:28-30,144-146` - the batched-stat
  shapes (`StatResult` and `StatxResult`) already carry `rdev_major` /
  `rdev_minor` so the metadata fans out fine from the flist; the loss
  happens later, at the writer.

A Windows oc-rsync receiver therefore advertises POSIX-shaped device entries
from the flist, runs the generator through its full create-or-replace
decision tree, but ultimately writes nothing to disk - and reports success.
The transfer summary, itemize lines, and exit code are all clean even
though the destination is missing the device, FIFO, or socket entries.

## Recommended strategy: skip-and-warn, with optional fake-super placeholder

Windows NT does not expose `mknod` semantics to non-driver user-mode code.
We have three realistic options.

1. Native NTFS reparse-point emulation (mirror Cygwin's layout).
   - Pro: byte-identical on-disk artefact when an oc-rsync transfer lands
     on a Cygwin install.
   - Con: requires producing privileged reparse-point payloads
     (`FSCTL_SET_REPARSE_POINT` with tag `IO_REPARSE_TAG_AF_UNIX` and
     friends), tying us to a Cygwin-private tag namespace, and likely
     wanting `SE_CREATE_SYMBOLIC_LINK_NAME` privilege. Round-trip parity
     with non-Cygwin Windows tools is impossible. Failure modes (UAC,
     anti-virus quarantines on reparse-tag writes) are user-hostile.
2. Skip-and-warn (RECOMMENDED).
   - Receiver-side: when a device/special entry is dequeued on Windows and
     no `--fake-super` was negotiated, emit a one-shot warning per entry
     using the `[receiver]` role trailer, increment a typed counter, and
     skip the entry. The wire side is untouched (sender still serialises
     rdev major/minor as today). Exit code follows upstream's
     `RERR_PARTIAL` policy when at least one entry was skipped.
3. Fake-super placeholder via the existing ADS-backed xattr backend.
   - `crates/metadata/src/xattr_windows.rs:1-50` already implements
     `list_attributes` / `read_attribute` / `write_attribute` /
     `remove_attribute` against NTFS Alternate Data Streams using
     `FindFirstStreamW` and `path:streamname:$DATA` open syntax.
   - When `--fake-super` is negotiated, create a 0-byte regular file at
     the destination path and write the upstream-format
     `"%o %d,%d %d:%d"` placeholder into a `user.rsync.%stat` ADS via
     that backend, exactly mirroring `xattrs.c:set_stat_xattr` (see
     citations above). A subsequent oc-rsync pull will rehydrate the
     synthetic stat via `get_stat_xattr`. This costs us nothing in new
     unsafe code: the ADS module is already wired into `--xattrs` via
     WPC-3.

The recommended default is option (2) plus opt-in (3). Option (1) is
documented as out-of-scope for the 0.x series and revisited when
`fast_io::windows::reparse` grows write support beyond WPC-8'.

Justification:

- Skip-and-warn is observable. Today's silent `Ok(())` violates the
  CLAUDE.md "Fail loud" rule; this audit's purpose was to surface that.
- Fake-super piggy-backs on shipping code (`xattr_windows.rs`,
  `xattrs.c:rsync_xal_set` parity). The mapping is upstream-compatible
  byte-for-byte and round-trips with Linux-side `--fake-super` peers.
- Option (1) requires either Cygwin reparse-tag fidelity (vendor-locked)
  or a fresh tag we own (interop-breaking). Neither buys enough to
  justify the implementation cost or the privilege requirement.

## Test plan (input to WIND-4)

WIND-4 must demonstrate:

1. Default Windows receiver behaviour - device or special entry in the
   incoming flist produces:
   - exit code matching upstream's partial-transfer outcome,
   - one warning per skipped entry with `[receiver]` role,
   - no inode created at the destination path,
   - no clobber of a pre-existing destination of any other type at the
     same path.
2. `--fake-super` Windows receiver behaviour - same input produces:
   - a regular 0-byte placeholder file at the destination,
   - a `user.rsync.%stat` ADS containing the upstream-format encoding
     for mode, rdev major, rdev minor, uid, gid,
   - byte-identical placeholder content to what an upstream `--fake-super`
     receiver would write to `tls.c:67-105`-formatted xattr on Linux.
3. Reverse direction - Windows sender reading a placeholder produced by
   (2) re-emits the original rdev/major/minor wire entry to a Linux
   receiver, which produces the real device node (root only).
4. Idempotency / re-run safety - running the transfer a second time on
   the same destination is a no-op (no duplicated warnings, no ADS
   churn, quick-check parity holds).
5. Cross-platform negative control - the existing Unix code paths in
   `crates/metadata/src/special.rs:292-305` and
   `crates/engine/src/local_copy/executor/special/device.rs:212-222` are
   unchanged.

## Out of scope for this audit

- The formal Windows-receiver state machine, error code mapping, and the
  CLI surface for the warning/skip behaviour: WIND-2.
- Code changes in `crates/metadata/`, `crates/engine/`,
  `crates/transfer/`, or `crates/flist/`: WIND-3.
- Regression tests asserting the behaviour spec'd above: WIND-4.
- Documentation in the Windows support matrix: WIND-5.
- Cygwin-native reparse-tag write support in `fast_io::windows::reparse`.
- Any change to wire encoding or to Unix-side semantics.
