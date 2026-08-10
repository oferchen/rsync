# Windows alternate data stream (ADS) handling strategy (WPC-2)

Tracks parent #2869 (Windows real-world parity series). Follows WPC-1
(#2903, audit doc `docs/audit/windows-ads-handling.md`) and feeds the
implementation in WPC-3 (#2905) and the regression test in WPC-4
(#2906).

Memory cross-links: `[[project_windows_real_world_parity_unclear]]`,
`[[project_xattr_acl_cross_platform_parity_gap]]`.

## 1. Decision

**oc-rsync adopts option (a), xattr-passthrough, as the official ADS
handling strategy on Windows. NTFS alternate data streams are surfaced
through the existing `-X` / `--xattrs` pipeline as
`user.windows.ads.<streamname>` xattr entries. Without `-X`, ADS are
silently dropped, exactly matching upstream rsync's Cygwin behaviour.
No new CLI flag, no new wire-protocol frame, and no new capability bit
is introduced. This is the binding strategy WPC-3 must implement.**

## 2. Options considered

| Option | Summary | Status |
|--------|---------|--------|
| (a) xattr-passthrough | ADS surface as `user.windows.ads.<streamname>` xattrs when `-X` is enabled; silently dropped otherwise (matches upstream default). | ALREADY IMPLEMENTED in `crates/metadata/src/xattr_windows.rs` (WPC-1 audit, section 3). |
| (b) `--ads` opt-in flag | Add an explicit oc-rsync-only flag that forces ADS round-trip even without `-X`. | Rejected. Adds a non-upstream-compatible flag and a duplicate CLI surface next to `-X`. |
| (c) Strip-by-default with no opt-in | Silently drop ADS even when `-X` is passed. | Rejected. Worst possible fidelity. Regresses current behaviour. |

## 3. Rationale for option (a)

- **Upstream compatibility.** Matches upstream rsync's documented
  default on Cygwin: without `-X`, ADS are not enumerated and not
  transferred. WPC-1 confirmed that upstream rsync 3.4.1 has zero
  ADS-aware code on any platform.
- **Existing infrastructure.** WPC-1 confirmed that
  `crates/metadata/src/xattr_windows.rs` already wraps
  `FindFirstStreamW`/`FindNextStreamW`/`FindClose` and routes
  enumeration, read, write, and remove through the cross-platform
  xattr layer in `crates/metadata/src/xattr.rs`. Zero new data-path
  code is required. WPC-3 only adds diagnostics and documentation.
- **Wire-protocol neutrality.** ADS streams are encoded as standard
  xattrs over the wire. No new `MSG_*` frame, no new capability-string
  flag, no new negotiated bit. A Windows oc-rsync sender talks to a
  stock Linux upstream rsync receiver using only the already-shipping
  xattr frames; the receiver applies them as ordinary POSIX user
  xattrs without needing ADS awareness.
- **User expectation.** A Windows user who explicitly passes `-X`
  reasonably expects "preserve all xattr-equivalent metadata".
  Surfacing ADS as xattrs satisfies that expectation without piling
  on a second flag with overlapping semantics.

## 4. What WPC-3 must implement (acceptance criteria)

WPC-3 is bound by the following concrete acceptance criteria. The
behavioural regression test that proves the round-trip is owned by
WPC-4 (#2906) and is deferred from this task.

1. **Round-trip verification (deferred to WPC-4).** Confirm via a
   regression test in WPC-4 (#2906) that
   `crates/metadata/src/xattr_windows.rs` performs the
   `user.windows.ads.<streamname>` <-> NTFS ADS round-trip on both
   read and write paths.
2. **One-shot verbose warning at the receiver.** Emit a single
   warning per transfer when ALL of the following hold:
   - The source is Windows (NTFS volume).
   - At least one source file carries a named ADS stream.
   - `-X` / `--xattrs` is NOT present in argv.
3. **Warning text format.** Exact text, emitted to STDERR via the
   standard logging path:
   ```
   warning: windows alternate data streams on %s will not be preserved without --xattrs (-X)
   ```
   `%s` is substituted with the offending source path. The warning
   must fire at most once per transfer, regardless of how many ADS-
   bearing files the walker discovers.
4. **Man-page entry.** Add a note in the EXTENDED ATTRIBUTES section
   of the man page describing the ADS-as-xattr mapping: that `-X`
   surfaces every named NTFS data stream as a
   `user.windows.ads.<streamname>` xattr entry, that the default `-a`
   matches upstream rsync on Cygwin by ignoring ADS, and that
   non-NTFS destinations (FAT32, exFAT) will fail to apply ADS on
   write.

## 5. Cross-platform receiver behaviour

Behaviour when a Windows-sourced `user.windows.ads.<streamname>` xattr
arrives at a non-Windows receiver:

- **Linux receiver.** Stored verbatim as
  `user.windows.ads.<streamname>` in the standard `user.*` xattr
  namespace. Visible via `getfattr -d <file>` and editable via
  `setfattr`. The existing xattr round-trip handles the value bytes
  transparently with no special case.
- **macOS receiver.** Also stored as `user.windows.ads.<streamname>`
  to keep the namespace stable across Unix-like receivers. The macOS
  xattr API tolerates arbitrary attribute names; we deliberately do
  not remap into the `com.apple.metadata:` family because that family
  carries macOS-specific semantics (Finder metadata, quarantine,
  Spotlight) and is not the right target for opaque Windows stream
  payloads.
- **Linux/macOS sender -> Windows receiver.** The Windows write path
  in `xattr_windows.rs` must strip the `user.windows.ads.` prefix
  before passing the bare stream name into `stream_path_wide`. WPC-3
  must verify (and WPC-4 must test) that this prefix-stripping is in
  place on the write path; the WPC-1 audit observed that the read
  path already produces bare stream names, so the prefix logic is
  the only new piece the Windows backend may need.

## 6. Rejected alternatives

- **(b) `--ads` opt-in flag.** Introduces a parallel CLI surface
  next to `--xattrs`. Users would have to ask "which one preserves
  my Zone.Identifier?" and answer differs by platform. It is also
  non-upstream-compatible: a stock upstream rsync binary on Cygwin
  has no `--ads` flag, so the CLI surface diverges with no
  matching wire-protocol divergence to justify it.
- **(c) Strip-by-default-even-with-X.** Silently regresses current
  behaviour. Today, an oc-rsync user who passes `-X` on a Windows
  source already gets ADS preservation. Removing that under the
  user's perceived full-metadata mode would lose Windows-source
  metadata without warning.

## 7. Rollback criteria

If WPC-3 / WPC-4 surface concrete failures in option (a), the
following pre-agreed rollback paths apply:

- **Round-trip Linux -> Windows reconstitution unsupported.** If
  `xattr_windows.rs` write path cannot reliably reconstitute an ADS
  from a `user.windows.ads.<streamname>` xattr (for example because
  the prefix-stripping logic does not exist and adding it would
  destabilise the existing Windows -> Windows path), fall back to
  option (b): ship an explicit `--ads` flag, mark the
  `user.windows.ads.*` xattr handling as a one-version deprecation,
  and document the migration in the release notes.
- **Write-side EACCES on Windows.** If the receiver-side ADS write
  fails on Windows with `EACCES` (or `ERROR_ACCESS_DENIED`) due to
  NTFS ADS permission semantics that we cannot satisfy from inside a
  standard xattr write, document the failure mode in the man page
  and add a `--no-ads` opt-out flag rather than changing the
  default. The default stays "preserve" because losing data silently
  is the worse failure mode.

## 8. Cross-references

- WPC-1 audit: `docs/audit/windows-ads-handling.md` (#2903, merged
  PR #4898).
- WPC-3 implementation: #2905 (pending; bound by section 4 above).
- WPC-4 regression test: #2906 (pending; owns the round-trip
  verification deferred from this spec).
- Parent: #2869 (Windows real-world parity series).
- Existing code surface: `crates/metadata/src/xattr_windows.rs`,
  `crates/metadata/src/xattr.rs`.
- CI coverage: `docs/design/windows-acl-xattr-ci-matrix.md` already
  pins the `windows-acl-xattr` job to keep `FindFirstStreamW` and the
  `:$DATA` suffix path exercised on every push.
- Memory: `[[project_windows_real_world_parity_unclear]]`,
  `[[project_xattr_acl_cross_platform_parity_gap]]`.
