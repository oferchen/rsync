# Windows Path Normalization Edge Cases

Survey of Windows-specific path-handling hazards beyond the wire-encoding
work already covered by `windows-path-normalization.md` and
`windows-path.md`. Each hazard is mapped to the concrete oc-rsync code
path and existing test coverage so future contributors know where the
defenses live and where they do not.

Last verified: 2026-05-01

Companion audits:

- `windows-path-normalization.md` (the path-form map and wire-encoding
  hazards F1-F6).
- `windows-path.md` (the closed `\` -> `/` wire-encoding fix).
- `windows_platform_parity.md` (which features are stubbed on Windows).

## Scope

Windows-specific path-handling concerns that survive the existing audits:
sanitization on the receiver, `\\?\` round-trips, reparse points,
reserved names, long paths, and drive-letter join semantics. Non-goals:
general path bugs, the already-tracked `\` -> `/` wire-encoding work,
and the symlink-on-Windows feature itself.

## Hazard inventory

| Hazard | Upstream rsync (Cygwin) | oc-rsync code | oc-rsync test | Gap? |
|---|---|---|---|---|
| Drive letter `C:\foo` | Cygwin re-maps to `/cygdrive/c/foo` before flist code sees it | `engine::local_copy::operands::operand_has_windows_prefix` (operands.rs:319) recognises drive prefix | `windows_operand_detection` mod (operands.rs:362+) | No - source-side recognition is correct |
| Drive-relative `C:foo` (no `RootDir`) | Cygwin treats colon as hostspec separator unless escaped; native drive-relative not supported | `Path::has_root()` returns false; receiver `sanitize_file_list` previously did not reject | new `windows_drive_relative_path_rejected_when_untrusted` test in this PR | **Closed in this PR** (see Findings W1) |
| UNC `\\server\share\foo` | Cygwin: `//server/share/...` preserved by `__CYGWIN__` block in `clean_fname` | `operand_has_windows_prefix` accepts; flist wire normalisation translates `\` -> `/` (`wire_path::path_bytes_to_wire`) | `wire_path::tests::windows_*` | No |
| `\\?\C:\foo` extended-length verbatim | Not supported by Cygwin | `operand_has_windows_prefix` accepts | `windows_operand_detection::extended_prefixes_are_local` | No |
| `\\?\UNC\server\share\foo` | n/a | `operand_has_windows_prefix` accepts | same as above | No |
| `\\.\` device namespace (e.g. `\\.\pipe\rsync`) | n/a | `operand_has_windows_prefix` accepts | `windows_operand_detection` | No - classified local; opening will fail at the OS, behaviour matches Cygwin's "unknown device" |
| Forward slash in Windows paths (`C:/foo`) | Cygwin accepts both | `Path::components` accepts; `wire_path::path_bytes_to_wire` only translates `\` not `/` | `wire_path::tests::windows_mixed_separators_are_normalized` | No |
| Backslash in path bytes on the wire | Cygwin always emits `/` because POSIX layer normalised first | Sender: `wire_path::path_bytes_to_wire` translates. Receiver: `PathBuf::from(&str_lossy)` on Windows accepts both `/` and `\` | `wire_path::tests::windows_*`, `accessors.rs:strip_leading_slashes` Windows branch | No - F1 from prior audit is closed |
| `Path::canonicalize` returns `\\?\`-prefixed result | Cygwin returns `/`-rooted POSIX path | `flist::file_list_walker::push_directory` (file_list_walker.rs:81) and the symlink follow at line 142 use the canonicalised form only as a visited-set key and as the next walk root; relative paths sent on the wire are built from `relative_prefix`, not the canonical form | none Windows-specific | Low risk - only `fs_path` becomes verbatim, not the relative path, but a future refactor that derives relative paths from `fs_path` would re-introduce the bug. Worth a comment. |
| `Path::strip_prefix(base)` divergence on `\\?\` vs non-verbatim base | n/a | `transfer::generator::file_list::walk` calls `path.strip_prefix(base).unwrap_or(&path)` at walk.rs:60, 282, 363. If `base` is non-verbatim and `path` got re-rooted on a verbatim-prefixed parent (via `canonicalize`) the strip silently fails and the entire `\\?\C:\...\file` is returned as the relative path | none | Latent bug. Today the only `canonicalize` in the walk path is for symlink follow and pushes the canonical form as the new walk root, so descendants strip from a matching base. Still worth a regression test. |
| Reserved names `CON`, `PRN`, `AUX`, `NUL`, `COM1-9`, `LPT1-9` | Cygwin same: kernel rejects open() | none | none | Behaviour matches upstream; no action |
| Trailing dots and spaces (`foo.`, `foo `) | Cygwin: NTFS kernel strips silently | none | none | Behaviour matches upstream; no action |
| Forbidden chars `<>:"\|?*` from a Linux sender | Cygwin: receiver fails on open() with ERROR_INVALID_NAME | flist read accepts arbitrary UTF-8 bytes; receiver fails at file create with `io::Error` | none | Behaviour matches upstream; failure mode is correct (transfer errors out) |
| Long paths > 260 without `\\?\` | Cygwin opens via NT path layer; supports long paths | None - the `flist::tests::path_max_limits` suite uses Linux/macOS PATH_MAX constants only; no Windows-specific 260 boundary test | `crates/flist/tests/path_max_limits.rs` | Latent: oc-rsync does not opt into Windows long-path support (no manifest, no `\\?\` injection). Files at depth >260 on a Windows receiver fail unless the user has set the `LongPathsEnabled` registry flag. Behaviour matches upstream. |
| Reparse points (symlinks, junctions, mount points) | Cygwin treats as POSIX symlinks where possible | `std::fs::symlink_metadata` reports `is_symlink()` for both true symlinks and surrogate reparse points; junctions return true here. The walker treats them uniformly. | `flist::tests::symlink_handling` | Junctions on Windows have different semantics from symlinks. Matching upstream behaviour, but worth documenting that follow-mode treats junctions as symlinks. |
| Case-insensitive comparisons | Cygwin defers to NTFS (case-insensitive) | All path comparisons in oc-rsync are byte-for-byte (case-sensitive). The `quick_check_ok_stateless` uses size+mtime, which is case-agnostic. | n/a | Matches upstream - NTFS preserves case but matches case-insensitively at the OS layer; oc-rsync's filename comparisons stay case-sensitive in the protocol layer (correct) and rely on the OS for case-insensitive filesystem matching (correct). |
| Symlink target absolute-path detection on Windows | Cygwin uses POSIX `/`-prefix | `transfer::symlink_safety::is_unsafe_symlink` only checks for `b'/'` first byte | none | F3 in prior audit (deferred until Windows symlink support lands). Still open. |

## Concrete findings

### W1. Receiver does not reject Windows drive-relative paths from an untrusted sender (HIGH severity, fixed in this PR)

**Failure mode.** A wire path like `C:foo` (drive + immediate name, no
`RootDir`) decodes on a Windows receiver into
`PathBuf::from("C:foo")`. `Path::has_root()` returns false because the
path lacks a `Component::RootDir`. The existing `sanitize_file_list`
rejection only checks `has_root()` and `..` components, so the entry
survives. Then `dest_dir.join(entry.path())` on Windows discards
`dest_dir` whenever the joined path carries a `Component::Prefix`,
yielding a destination of `C:foo` regardless of where the user pointed
their `--dest`. The same vector also matches `C:\absolute`,
`\\server\share\file`, and `\\?\C:\verbatim` if a sender had a way to
emit them on the wire.

**Reproducer.** With `trust_sender = false` (the default for daemon
mode), construct a `FileEntry` with name `C:foo` and run it through
`sanitize_file_list`. Before this PR, the entry survives. After, it is
rejected with `rejecting file-list entry with Windows drive or UNC
prefix from sender`.

**Fix applied in this PR.** Added a `#[cfg(windows)]` check inside
`sanitize_file_list` that rejects any path whose first component is a
`Component::Prefix(_)`. Single file (`receiver/file_list.rs`), single
crate (`transfer`), single concept (untrusted-sender rejection),
< 30 LoC. Ships with two new Windows-only unit tests in
`receiver/tests.rs`.

**Why this is a Windows-only check.** Upstream rsync runs on Windows
only via Cygwin, whose POSIX layer never lets these forms reach the
sender. On native Win32 the layer doesn't exist, and the receiver is
the last place to enforce the invariant. The check is gated on
`#[cfg(windows)]` to avoid altering Linux/macOS behaviour.

### W2. Walker uses canonicalized `\\?\` paths as descent roots on Windows (LOW severity, not fixed)

**Failure mode.** `flist::file_list_walker::push_directory` calls
`fs::canonicalize(&fs_path)` at line 81 and uses the result as a
visited-set key. The follow-symlinks branch at line 142 also calls
`canonicalize` and uses that result as the next directory's
`fs_path`. Today this is safe because the relative path sent on the
wire is built from `relative_prefix` (not from the canonical
`fs_path`), and the visited-set is platform-agnostic.

**Latent risk.** A future refactor that derives `relative_path` from
`fs_path.strip_prefix(root)` would silently begin emitting verbatim
prefixes over the wire on Windows, since the canonicalised root would
be `\\?\C:\...` while the user's `root` argument typically is not.

**Recommendation.** Document the invariant inline. No fix today.

### W3. Long paths beyond MAX_PATH on Windows are not opted-in (INFORMATIONAL)

**Failure mode.** Windows kernel APIs reject paths > 260 chars unless
the application either prefixes them with `\\?\` or opts in via the
manifest (`LongPathsEnabled` registry key). oc-rsync does neither.
A receiver running on Windows will fail with `ERROR_PATH_NOT_FOUND` for
sufficiently deep transfers from a Linux sender.

**Behaviour vs upstream.** Cygwin's POSIX layer routes through NT
paths internally and supports long paths. oc-rsync on native Win32 does
not. The mismatch is documented for completeness only.

**Recommendation.** Consider adding a Windows app-manifest fragment
that sets `longPathAware`, plus integration tests that build trees
deeper than 260 chars under a Windows runner. Out of scope for this
audit.

### W4. Junctions and reparse points are walked as symlinks (INFORMATIONAL)

`fs::symlink_metadata().file_type().is_symlink()` returns true for
both true Windows symlinks and surrogate reparse points (junctions,
mount points). The walker therefore follows junctions when
`follow_symlinks` is set. Matches the typical user expectation but
diverges from native rsync, which on Cygwin sees only POSIX symlinks.

**Recommendation.** Document that on Windows, `--copy-links`
transitively follows junctions. No code change.

## Recommended fixes (ranked by severity)

1. **W1 (HIGH, fixed in this PR).** Reject `Component::Prefix` on the
   Windows receiver inside `sanitize_file_list`.
2. **W2 (LOW).** Add an inline comment in `file_list_walker.rs`
   spelling out that `fs_path` may carry a verbatim prefix on Windows
   and must not be used to derive wire-form relative paths.
3. **W3 (INFORMATIONAL).** Add a Windows app-manifest opting into long
   paths, plus a regression test under the Windows CI runner.
4. **W4 (INFORMATIONAL).** Document junction-as-symlink behaviour in
   `windows_platform_parity.md`.

## Out of scope

- Symlink target absolute-path detection on Windows (F3 in
  `windows-path-normalization.md`). Tied to the symlink-on-Windows
  feature itself.
- Promoting `engine::local_copy::operand_is_remote` to a shared helper
  (F2 from the prior audit). Touches multiple crates.
- WTF-8 / non-UTF-8 filename round-trip (F4 and F5 from the prior
  audit). Future hardening.

## References

- Upstream `flist.c:757` `clean_fname(thisname, CFN_REFUSE_DOT_DOT_DIRS)`.
- Upstream `util1.c:955-961` `__CYGWIN__` two-leading-slash preservation.
- Rust `std::path::Component::Prefix` and the `windows-msvc`
  documentation on `Path::join` semantics for prefixed inputs.
- oc-rsync `crates/transfer/src/receiver/file_list.rs::sanitize_file_list`.
- oc-rsync `crates/protocol/src/flist/wire_path.rs`.
- oc-rsync `crates/engine/src/local_copy/operands.rs::operand_has_windows_prefix`.
- oc-rsync `crates/flist/src/file_list_walker.rs::push_directory`.
