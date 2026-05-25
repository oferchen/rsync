# Windows alternate data stream (ADS) handling audit (WPC-1)

Tracks parent #2869 (Windows real-world parity series). Feeds follow-up
#2904 (WPC-2: choose the strategy and ship a spec).

## 1. Background

NTFS allows every file to carry multiple named data streams. The
default unnamed stream (`::$DATA`) is what every POSIX-style API
reads and writes as the file body. Additional streams attach extra
data to the file without affecting its visible size or path.

- Win32 exposes the streams via the `file.txt:streamname` syntax
  passed to `CreateFileW`, plus the dedicated
  `FindFirstStreamW`/`FindNextStreamW` pair for enumeration. The
  internal type tag for the user-visible data flavour is `$DATA`,
  so a fully qualified stream path is `file.txt:streamname:$DATA`.
- Real-world producers of named streams: Microsoft browsers and Office
  write `Zone.Identifier` (Mark-of-the-Web) and `OECustomProperty`
  streams; backup tools and AV scanners write provenance streams;
  Cygwin and WSL2 mount-helpers occasionally surface POSIX metadata
  shadows; deliberate user-authored streams via PowerShell's
  `-Stream` parameter on `Get-Content`/`Set-Content`.
- Non-NTFS volumes (FAT32, exFAT, ReFS in some modes) do not store
  ADS. `FindFirstStreamW` returns `ERROR_HANDLE_EOF` immediately on
  those volumes.

## 2. Upstream rsync behaviour on Cygwin

Source tree audited: `target/interop/upstream-src/rsync-3.4.1/`.

`grep -rn` across the upstream source for any of
`FindFirstStream`, `FindNextStream`, `BackupRead`, `BackupWrite`,
`alternate data stream`, `streamname`, `Zone.Identifier`, or
`:$DATA` returns no hits in any `.c`, `.h`, `.in`, `.m4`, or
`configure*` file. Upstream rsync has no ADS-aware code path.

The Cygwin port is opportunistic: the only `__CYGWIN__` blocks in
the upstream tree are:

- `rsync.h:587` - feature gate for `SUPPORT_CRTIMES` when the
  toolchain advertises either `HAVE_GETATTRLIST` or `__CYGWIN__`.
- `syscall.c:69,457,465` - Cygwin reaches `crtime` via Win32
  `SetFileTime` instead of macOS `getattrlist`/`setattrlist`. No
  stream enumeration.
- `util1.c:955` - preserves the leading `//` on a path so
  `\\server\share` UNC paths survive normalisation.
- `rsync.c:623` - selects the Cygwin `do_SetFileTime` branch when
  applying create-times.
- `configure.ac:420,961,1390` - Cygwin host detection and
  iconv-library naming.
- `NEWS.md:1957` - changelog entry for the `//` preservation only.

Cygwin's POSIX layer translates `open()` into `CreateFileW` against
the unnamed `::$DATA` stream, so upstream rsync running on Cygwin
copies the file body and nothing else. ADS attached to the source
are silently dropped on the destination; ADS attached to the
destination but not the source are not deleted by `--delete`
because they are invisible to the enumeration that drives deletion.
Upstream's `xattrs.c` backend (`lib/sysxattrs.{c,h}`) ships only
`HAVE_LINUX_XATTRS` / `HAVE_SYS_EXTATTR_H` (BSD) / `__APPLE__`
implementations and a `SUPPORT_XATTRS`-undefined stub for everything
else, Cygwin included.

Conclusion: an upstream rsync binary on Cygwin is wire-compatible
with stock rsync, ignores ADS entirely, and offers no opt-in flag
to round-trip them. Any oc-rsync behaviour that goes further is an
extension, not an interop divergence.

## 3. Current oc-rsync state

oc-rsync already implements ADS-as-xattrs end to end. The relevant
code lives in `crates/metadata/src/`:

- `xattr_windows.rs` (whole file). Wraps `FindFirstStreamW`,
  `FindNextStreamW`, and `FindClose` to enumerate every named data
  stream on a path. Uses the constant `STREAM_SUFFIX = ":$DATA"`
  at line 64 to recognise the type tag and strip it off so the
  cross-platform layer sees bare attribute names. The unnamed
  primary stream is skipped explicitly (`parse_stream_name`
  returns `None` for `::$DATA`, line 132). Read, write, and remove
  use `CreateFileW` against `path:name:$DATA` paths built by
  `stream_path_wide`. The module is gated `#[cfg(windows)]` from
  `crates/metadata/src/lib.rs:146`.
- `xattr.rs:10` documents the mapping in the module rustdoc:
  "On Windows the [`xattr_windows`] module maps every named xattr
  onto an NTFS Alternate Data Stream (`path:name:$DATA`) so the
  client/daemon surface remains the same." The `#[cfg(windows)]
  use crate::xattr_windows as backend;` at line 32 routes all
  cross-platform xattr calls (`list_attributes`, `read_attribute`,
  `write_attribute`, `remove_attribute`,
  `read_xattrs_for_wire`, `apply_xattrs_from_wire`) through the
  Windows backend when the target is Windows.
- `is_xattr_permitted` (line 64-67) returns `true` for every name
  on non-Linux targets; the namespace filtering that gates `user.*`
  on Linux non-root is skipped because NTFS ADS is a single flat
  namespace without `user.` / `system.` distinctions.

`fast_io` and the engine's local-copy path contain no ADS-aware
code. A `grep` across `crates/fast_io/src/` and
`crates/engine/src/` for `FindFirstStream`, `FindNextStream`,
`BackupRead`, `Zone.Identifier`, `streamname`, or `:$DATA`
returns no hits. The copy fast paths
(`copy_file_range`, `copy_file_ex`, `macos_io`) only ever touch
the unnamed primary stream, which is the correct behaviour because
ADS round-trip is owned by the xattr layer in oc-rsync's design.

The existing CI plan and integration tests already account for ADS:
`docs/design/windows-acl-xattr-ci-matrix.md` lines 18-23, 28, 54,
70, 90, 103, 126 list `#1867` (Windows xattrs / ADS) as done and
pin the `windows-acl-xattr` job to keep
`FindFirstStreamW` and the `:$DATA` suffix path exercised on every
push.

## 4. Risk surface

The risk shape depends on what the user passes on the command line.

- **Default (`-a` / no `-X`).** xattr capture is off, so even though
  the Windows backend is compiled in, it is never called.
  Behaviour matches upstream rsync on Cygwin exactly: only the
  unnamed primary stream is read, named ADS on the source are
  silently dropped on the destination, and `--delete` cannot
  observe ADS on the destination because it never asks. This is
  silent data loss for any payload riding on named streams
  (`Zone.Identifier`, Outlook `OECustomProperty`, etc.) but it is
  the same silent data loss upstream produces, so there is no
  interop divergence to report.
- **`-X` / `--xattrs`.** The Windows backend wakes up.
  `list_attributes` enumerates every named ADS on the source and
  surfaces it through the standard xattr wire format. On the
  receiver side those entries are reapplied via
  `apply_xattrs_from_wire` -> `write_attribute`, which opens
  `path:name:$DATA` with `CREATE_ALWAYS | GENERIC_WRITE` and
  rewrites the stream payload. Cross-platform direction matters:
  - Windows -> Windows: lossless, byte-for-byte round trip.
  - Windows -> Linux/macOS: ADS bytes land in POSIX xattrs.
    `Zone.Identifier` becomes a regular xattr value on Linux. No
    data loss but the name lands in the platform's flat namespace
    and Linux non-root callers will trip the `user.` filter at
    `xattr.rs:64`.
  - Linux/macOS -> Windows: POSIX xattrs land as ADS. Names
    containing `:` or NUL are rejected by `stream_path_wide` at
    `xattr_windows.rs:99-103` with `InvalidInput`; everything else
    round-trips.
  - Underlying volume must be NTFS. FAT32/exFAT destinations cause
    `write_attribute` to fail; the error propagates up the
    transfer error chain.
- **Error modes already covered.** Non-UTF-8 stream names from a
  hostile source are coerced via `to_string_lossy`
  (`xattr_windows.rs:73`) rather than dropped silently. Missing
  streams during read or remove map to `Ok(None)` / `Ok(())`,
  mirroring POSIX `xattr::get` semantics.
- **Risk that remains.** Without an explicit `-X`, a user moving
  data off a Windows box has no signal that ADS exist on the
  source. The default `-a` does not warn even when
  `Zone.Identifier` is widespread (post-Edge / post-Outlook
  installs ship them on most downloaded files). This matches
  upstream behaviour, but for an oc-rsync user the missing signal
  is the dominant complaint vector.

## 5. Options for WPC-2

Three strategies are on the table for WPC-2 to pick from.

- **(a) xattr passthrough, current behaviour.** Keep ADS routed
  through `-X`. No new flag, no namespace prefix beyond what NTFS
  reports. Already shipped. Cost is zero. Drawback is that users
  who do not pass `-X` get silent loss, identical to upstream but
  unhelpful. Optional refinement: synthesise a
  `user.windows.ads.<streamname>` prefix on the wire so non-NTFS
  receivers can distinguish ADS-origin xattrs from genuine POSIX
  xattrs, at the cost of breaking interop with upstream rsync on
  Cygwin (which would now see a foreign-namespace xattr it cannot
  apply).
- **(b) Strip-by-default with `--ads` opt-in.** Add a Windows-only
  flag that captures ADS into xattrs (effectively shorthand for
  `-X` plus a namespace filter that only the `xattr_windows`
  backend honours). Default unchanged. Cost is a new CLI surface,
  a new conformance test matrix, and a documentation entry. No
  wire-protocol changes if the flag desugars to existing xattr
  bytes.
- **(c) Document as a known limitation, do nothing.** Already
  satisfied by `docs/design/windows-acl-xattr-ci-matrix.md` and
  this audit. Cost is zero. Drawback is the same silent-loss
  complaint vector as (a).

## 6. Recommendation

Pick option **(a)** as the WPC-2 spec target, with two narrow
deliverables on top of what is already in tree:

1. A user-facing note in the man page (and the `--help` text for
   `-X` on Windows) calling out that `-X` is required to round-trip
   NTFS alternate data streams, and that the default `-a` matches
   upstream rsync on Cygwin by ignoring them.
2. A small warning emitted by the Windows sender when `-X` is
   omitted and the source root contains at least one ADS-bearing
   file. The check piggybacks on the existing flist walk
   (`FindFirstStreamW` is already linked) and is gated behind a
   non-default verbose level so quiet transfers stay quiet.

Rationale:

- Upstream compatibility: `(a)` preserves byte-for-byte interop
  with upstream rsync on Cygwin under default flags and under
  `-X`. `(a)`'s optional `user.windows.ads.` namespace prefix is
  rejected on those grounds: it would force a divergent xattr name
  on the wire that a stock upstream binary would refuse or
  misapply.
- User expectation: the dominant complaint is "I copied my Windows
  tree and lost `Zone.Identifier`". A man-page note plus an opt-in
  warning addresses that without adding a Windows-only CLI flag
  whose semantics duplicate `-X`.
- Complexity: zero new wire bytes, zero new flags, one help-text
  change, one verbose-only warning. Fits the standing rule against
  inventing protocol features for niche workloads. The existing
  `windows-acl-xattr` CI job already covers the regression
  surface, so no new matrix entry is needed.

Option **(b)** is the fallback if downstream feedback after WPC-2
ships shows that the `-X` opt-in is too opaque. Adding `--ads` is
a strict superset of (a) and can land later without disturbing the
wire format.

Option **(c)** is rejected: the audit itself is doc work, but the
silent-loss vector deserves the small CLI/man-page tweak that (a)
adds.
