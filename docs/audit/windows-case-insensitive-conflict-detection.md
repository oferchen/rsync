# NTFS case-insensitive conflict-detection audit (WPC-11)

Tracks parent #2869 (Windows real-world parity series). Companion to
the WPC-13 Windows support matrix
(`docs/user/windows-support-matrix.md`, PR #4920), the WPC-5 long-path
audit (`docs/audit/windows-long-path-support.md`, PR #4940), and the
WPC-7 reparse-point classification audit
(`docs/audit/windows-reparse-point-classification.md`, PR #4937). Feeds
the WPC-11 follow-up implementation work that ships the regression
test specified in section 7. Memory notes inline:
[[project_windows_real_world_parity_unclear]],
[[project_windows_parity_wip]].

## 1. Scope

WPC-11 audits whether oc-rsync detects and handles case-collisions
that arise when a source tree carrying two case-distinct entries
(`Makefile` and `makefile`, `README.txt` and `Readme.txt`) is
transferred to a destination on a case-insensitive NTFS volume. The
audit covers:

- Every call site under `crates/transfer/src/`, `crates/engine/src/`,
  and `crates/metadata/src/` that performs case-fold comparison or
  consults the destination filesystem's case-sensitivity flag.
- Every `io::ErrorKind::AlreadyExists` handler that could plausibly
  surface a case-collision today.
- Upstream rsync 3.4.1 behaviour on Cygwin and any native Windows
  port, to anchor the compatibility ceiling.
- Four candidate strategies, a recommendation, and a concrete
  regression-test specification for the WPC-11 follow-up to land.

Out of scope: the source-side flist ordering on case-sensitive
filesystems (already byte-for-byte sorted via
`compare_file_names` - see section 3); the per-directory NTFS
case-sensitive flag enable / disable workflow (an admin opt-in, not a
runtime decision oc-rsync makes); the WSL `DrvFs` case-sensitivity
projection (`drvfs.options=case=dir`) which sits outside the native
Win32 path.

## 2. Background on NTFS case sensitivity

The rules relevant to this audit:

- **NTFS is case-preserving**. Every filename is stored verbatim with
  its original case. A user who creates `Makefile.txt` sees that case
  in `dir` and `FindFirstFileW` output for the rest of the file's life.
- **NTFS is case-insensitive by default**. The Win32 path-parsing
  layer applies `OBJ_CASE_INSENSITIVE` to every `CreateFileW`,
  `DeleteFileW`, `GetFileAttributesW`, and friends. A handle open on
  `makefile.txt` returns the existing `Makefile.txt` and a subsequent
  write to it overwrites that file in place. The two names alias to
  the same `$MFT` entry.
- **`FILE_FLAG_POSIX_SEMANTICS` does not flip case sensitivity at the
  open call**. That flag controls delete-share semantics. Per-handle
  case-sensitive open requires an `NtCreateFile` call without the
  `OBJ_CASE_INSENSITIVE` attribute, which is not exposed through the
  Win32 path-accepting API surface.
- **Per-directory case-sensitive flag (Windows 10 1809+)**. The
  filesystem driver supports a per-directory `FILE_CS_FLAG_CASE_SENSITIVE_DIR`
  toggled by `FSCTL_SET_CASE_SENSITIVE_INFO`
  (`FileCaseSensitiveInformation` info class). When set, both the
  directory entry lookup and the child creation honour case. The flag
  requires admin rights to set, requires the "Windows Subsystem for
  Linux" optional component to be enabled, and inherits to new
  children but not to existing ones. Default NTFS volumes ship with
  the flag off on every directory.
- **`fsutil.exe file setCaseSensitiveInfo <dir> enable`** is the
  user-facing entry point that wraps the FSCTL. Most production
  Windows hosts never invoke it.
- **Source filesystems that produce case-distinct pairs**.
  ext4, xfs, btrfs, zfs, and case-sensitive APFS all permit `Makefile`
  and `makefile` to coexist. Many open-source repositories (`make`'s
  own test fixtures, the Linux kernel `Documentation/networking/`
  tree, several Go modules) carry such pairs. Pull-from-Linux to
  push-to-NTFS is therefore a real-world scenario, not a theoretical
  one.

## 3. Inventory of case-conflict detection in oc-rsync

`ripgrep` across the three crates the task brief calls out
(`crates/transfer/src/`, `crates/engine/src/`,
`crates/metadata/src/`) for the keyword set
`case.insensitive | eq_ignore_ascii_case | ascii_lowercase | to_lowercase | case.collision | case.fold`
returns every hit below:

- `crates/metadata/src/acl_windows/posix_map.rs:59-60` -
  `ace.ace_type.eq_ignore_ascii_case("A")` /
  `.eq_ignore_ascii_case("D")` - matches ACE-type literals (`A` for
  allow, `D` for deny) in the POSIX-ACL-to-Windows-ACL mapper.
  Unrelated to filename casing.
- `crates/metadata/src/acl_exacl/error.rs:35` -
  `e.to_string().to_lowercase()` - case-folds an error string for
  classification. Unrelated to filename casing.
- `crates/engine/src/local_copy/dir_merge/load.rs:159,183,255` -
  `token.to_ascii_lowercase()` / `directive.to_ascii_lowercase()` /
  `.eq_ignore_ascii_case("clear")` - filter-rule keyword parser.
- `crates/engine/src/local_copy/dir_merge/parse/{modifiers,dir_merge,merge,line}.rs` -
  all `eq_ignore_ascii_case` / `to_ascii_lowercase` hits parse
  filter-directive keywords (`merge`, `clear`, `include`, `exclude`,
  `show`, `hide`, `protect`, `risk`, `dir-merge`).
- `crates/engine/src/local_copy/skip_compress.rs:70,127,144,165` -
  `ext.to_ascii_lowercase()` / `byte.to_ascii_lowercase()` -
  normalises file extensions for the `--skip-compress` list.
- `crates/engine/src/concurrent_delta/spill/env.rs:78` -
  `raw.trim().to_ascii_lowercase()` - normalises a spill-policy
  environment-variable string.
- `crates/engine/src/local_copy/executor/file/sparse/mod.rs:72,79` -
  case-insensitive parse of the `OC_RSYNC_SPARSE_STRATEGY` value.

No hit performs a case-fold comparison between a source path and a
destination filename. No hit stats the destination directory for a
case-different existing entry. The `crates/engine/src/local_copy/executor/directory/support.rs:140-162`
`compare_file_names` helper (used during sorted traversal) is
explicitly byte-for-byte case-sensitive on both Unix (`as_bytes`) and
Windows (`encode_wide`); it is annotated `// upstream: flist.c:file_compare()`.

`ripgrep` for the Windows-specific case-sensitivity APIs
(`FILE_FLAG_POSIX_SEMANTICS`, `SetCaseSensitiveInfo`,
`FileCaseSensitiveInfo`, `FileCaseSensitiveInformation`,
`FSCTL_SET_CASE_SENSITIVE_INFO`, `OBJ_CASE_INSENSITIVE`,
`NtSetInformationFile`) across the workspace returns no matches.
oc-rsync never queries nor toggles the per-directory case-sensitive
flag.

`ripgrep` for `ERROR_FILE_EXISTS` (the Win32 `0x50` that
`io::ErrorKind::AlreadyExists` wraps on Windows) returns no direct
match. Every `AlreadyExists` consumer pattern-matches on
`io::ErrorKind::AlreadyExists` instead. The handlers fall into four
non-case-aware buckets:

- **Temp-file name collisions (benign retry)**: 
  `crates/transfer/src/temp_guard.rs:183` rolls a fresh random suffix
  inside the `MAX_OPEN_ATTEMPTS = 100` loop. 
  `crates/engine/src/local_copy/executor/file/guard.rs:203` does the
  same on the staged-write path.
- **`mkdir` of an existing directory (silently OK)**:
  `crates/transfer/src/receiver/directory/creation.rs:286` and
  `crates/engine/src/local_copy/context_impl/state.rs:525` swallow
  `AlreadyExists` because the parent dir already exists.
- **Symlink / FIFO / device target already present (remove and
  retry)**: 
  `crates/engine/src/local_copy/executor/special/symlink.rs:326`,
  `crates/engine/src/local_copy/executor/special/fifo.rs:148`,
  `crates/engine/src/local_copy/executor/special/device.rs:146`, and
  `crates/engine/src/local_copy/executor/file/copy/links.rs:{83,153,299}`
  unlink the existing entry and reissue the create.
- **Rename target already present at commit (remove and retry)**:
  `crates/engine/src/local_copy/executor/file/guard.rs:325` calls
  `remove_existing_destination` and re-renames.

The remove-and-retry path is the one a case-collision would traverse
in the rare event the second source file goes through the staged
temp-file commit rather than overwriting in place. On NTFS the rename
of `__oc_tmp_XYZ` -> `README.txt` succeeds without surfacing
`AlreadyExists` for an existing `Readme.txt` because the destination
namespace is case-insensitive - the kernel sees no collision and
overwrites the entry in place. No code path along the receiver,
generator, or local-copy executor inspects the final inode's stored
casing afterwards.

**The complete case-collision inventory is therefore: zero detection
sites, zero remediation sites.** Section 8 codifies this as finding
F1.

## 4. Upstream rsync behaviour on Cygwin

`ripgrep` of `target/interop/upstream-src/rsync-3.4.1/` for
`collision`, `case_conflict`, `EEXIST` next to receiver / generator
code, and `__CYGWIN__` / `_WIN32` blocks returns:

- `syscall.c:69` / `syscall.c:457-490` - the only `__CYGWIN__` blocks
  in receiver-adjacent code handle `crtime` extraction
  (`do_SetFileTime` via `CreateFileW` + `SetFileTime`). No case logic.
- `util1.c:955` - a `__CYGWIN__` block in path-canonicalisation
  helpers. No case logic.
- `rsync.c:623` - a `__CYGWIN__` block in the umask helper. No case
  logic.
- `lib/wildmatch.c:84` - `t_ch = tolower(t_ch)` inside `wildmatch`
  when the `WM_CASEFOLD` flag is set. Filter pattern matching, not
  destination filesystem reconciliation.
- `token.c:159,199,260` - `toLower` inside the compress / decompress
  symbol-table maintenance. Unrelated.
- `authenticate.c:263-265` - `toLower` for the daemon-auth option
  character. Unrelated.
- `loadparm.c:281` - case-insensitive whitespace-ignoring `string_equal`
  for daemon-config keyword lookup. Unrelated.

Receiver-side `EEXIST` handling in `backup.c:102`, `backup.c:206`,
`backup.c:247`, `util1.c:213`, `util1.c:224`, `generator.c:1472`, and
`generator.c:1475` covers backup-file collision and `do_mkdir`
idempotence respectively. None of those sites compare the existing
inode's name casing against the source file's name.

Upstream rsync's `make_file` / `recv_files` path opens the destination
via the plain POSIX `open(name, O_WRONLY|O_CREAT|O_TRUNC, mode)`. On
Cygwin that call dispatches through Cygwin's `path_conv` which honours
the Cygwin-mount case sensitivity setting (default: case-insensitive
to match Windows). The second write of a case-distinct pair therefore
opens the first file's existing handle, truncates, and rewrites it.
Upstream surfaces no warning. The final filename on disk retains the
case of whichever file the source enumerator produced *first* (because
the in-place rewrite preserves the existing $MFT entry's case), with
the *contents* of whichever file was written second.

The native Windows port (`rsync.exe` distributed via msys2 / cwrsync)
inherits the same POSIX-call wrapper, so the result is identical: no
detection, second-write contents wins, first-write casing wins.

The upstream compatibility ceiling for oc-rsync is therefore "do not
worse than upstream". Adding detection is a defensible extension as
long as no wire-protocol behaviour changes and the default user
experience remains unchanged when transferring to case-sensitive
filesystems.

## 5. Expected behaviours to consider

Four strategies are viable. Each is evaluated against three axes:
upstream compatibility, user-visible signal, and data-loss prevention.

### (A) Pass-through (default upstream)

Do nothing. The second write opens the same NTFS entry, truncates,
and rewrites. The first source file's contents are lost; the
destination filename retains the first source file's case; the user
sees no diagnostic. This is exactly upstream's behaviour.

- Upstream compat: identical.
- Signal: none.
- Data loss: yes, silently.

### (B) Detect-and-warn

Before opening the destination file for write, the receiver stats the
parent directory (or `GetFileAttributesW`-equivalent on the
destination path's parent) and case-folds the existing entries. If a
case-different entry matches the source basename, the receiver logs a
warning to stderr (existing `info_log!` or `warning!` macro,
classifier `IRRELEVANT`-equivalent to `--info=NAME` so users can opt
out), then performs the write as before. The destination ends with
the same single-file state as strategy A, but the user has a
diagnostic to act on.

- Upstream compat: identical on the wire and at the filesystem; one
  extra stderr line.
- Signal: yes, per collision.
- Data loss: yes, but documented.

### (C) Detect-and-skip

Same detection as (B), but on collision the receiver skips the second
write, logs the skip, and bumps a per-run skip counter such that the
process exits non-zero (`ExitCode::Partial`, exit 23) if any skip
fired. The destination state is "first source file wins" instead of
"second source file wins for content, first for case".

- Upstream compat: diverges - upstream silently overwrites, oc-rsync
  silently keeps the first. Cross-tree round-trip parity tests
  comparing oc-rsync vs upstream output would fail on case-conflict
  inputs.
- Signal: yes.
- Data loss: prevented for the first source file; the second source
  file's payload is dropped.

### (D) Detect-and-rename

Same detection as (B), but on collision the second write goes to a
disambiguated name (e.g. `README~CASEDUP.txt`). Both files end up on
disk. The receiver logs the rename.

- Upstream compat: divergent in a more surprising way (filenames on
  destination differ from source).
- Signal: yes.
- Data loss: prevented, at the cost of a confusing destination tree
  and downstream tools (`make`, `git`, build systems) seeing a name
  rsync invented.

## 6. Recommendation

Adopt **strategy (B) detect-and-warn**.

Rationale:

- **Upstream compatibility is preserved.** No wire-format change. The
  destination filesystem state matches upstream byte-for-byte. Round-
  trip tests against `rsync-3.4.1` and `rsync-3.4.2` continue to pass.
- **The user gains a signal.** The transfer succeeds, the user learns
  which pair collided, and the warning carries enough context
  (`source-path -> dest-path conflicts with existing dest-path/Alias`)
  for them to decide whether to enable the per-directory
  case-sensitive flag, exclude one of the pair, or accept the loss.
- **Strategy (C) silently changes destination semantics**, breaks
  interop parity, and fails the "do not worse than upstream" rule when
  the user expected upstream behaviour. Strategy (D) invents
  destination filenames that no other tool understands. Strategy (A)
  is the status quo and the gap WPC-11 was opened to address.
- **The detection cost is bounded.** A single `read_dir` of the
  destination's parent per write that finds a case-fold match is `O(n)`
  in the parent directory's entry count. On hot paths (many small
  files per directory) the cost is non-trivial; the implementation
  should cache the parent listing per receiver-side directory cursor
  (the generator already maintains one for incremental recursion).
- **Cross-platform behaviour is consistent.** The same code path runs
  on Linux destinations mounted on case-insensitive volumes (vfat,
  ntfs-3g without `windows_names`, exfat, case-insensitive APFS),
  surfacing the same warning. macOS HFS+ default volumes are
  case-insensitive too; the warning fires there as well.

## 7. Regression test specification

Test file: `crates/transfer/tests/wpc_11_case_collision.rs`.

### Source fixture

A `tempfile::TempDir` containing two case-distinct regular files,
created via `std::fs::File::create`:

- `Makefile` (contents `b"upper\n"`)
- `makefile` (contents `b"lower\n"`)

Source creation is gated `#[cfg(unix)]` because creating two
case-distinct files in the same directory requires a case-sensitive
source filesystem. macOS users running default case-insensitive APFS
are skipped via an early-return guard that probes whether the second
`File::create` succeeds.

### Destination fixture

A second `tempfile::TempDir`. The destination filesystem is whatever
the test host provides:

- On Linux runners (`ext4` / `tmpfs`): case-sensitive; both source
  files land as separate destination entries.
- On macOS runners (default APFS): case-insensitive; only one
  destination entry survives.
- On Windows runners (NTFS): case-insensitive; only one destination
  entry survives.

### Transfer invocation

Use the public `oc_rsync::core::session` API (or
`transfer::tests::harness::run_local`, whichever the sibling
`crates/transfer/tests/*` tests prefer) with:

- `recursive = true`
- `local = true` (no daemon, no SSH)
- source = source-dir path
- destination = destination-dir path

Capture stderr by routing the session logger through an
`InMemoryStderr` writer (already used by sibling tests under
`crates/transfer/tests/`).

### Assertions

Common (every platform):

- Transfer completes with `ExitCode::Success` (strategy B does not
  change the exit code).
- The source dir still contains both `Makefile` and `makefile` -
  verifies the test created them.

Case-sensitive destination (Linux runners; detected by re-running the
case-distinct create probe on the destination dir):

- Destination contains exactly two entries: `Makefile` and `makefile`.
- Destination `Makefile` has contents `b"upper\n"`.
- Destination `makefile` has contents `b"lower\n"`.
- Captured stderr contains no case-collision warning.

Case-insensitive destination (Windows, macOS-default-APFS, Linux on
exfat/vfat mounts):

- Destination contains exactly one entry, name equals whichever source
  file the receiver enumerated first (Unix `sort` order: `Makefile`
  comes before `makefile` because `'M' (0x4D) < 'm' (0x6D)`; assert
  the surviving entry is `Makefile`).
- The surviving entry's contents equal `b"lower\n"` (the second write
  overwrote in place, per strategies A and B).
- Captured stderr contains exactly one case-collision warning whose
  text mentions both source basenames (`Makefile` and `makefile`) and
  the destination path. Test asserts the substrings `"Makefile"`,
  `"makefile"`, and `"case-conflict"` (final wording to match the
  implementation; the assertion is keyword-based, not full-line).

### Cross-platform gate (source fixture probe)

```text
// Source needs case-sensitivity to create the pair.
let probe = src_dir.join("Probe");
let probe_lower = src_dir.join("probe");
File::create(&probe).expect("source probe upper");
if File::create(&probe_lower).is_err() {
    // Source filesystem is case-insensitive (default APFS on the
    // test host). The fixture cannot be built; skip without failure.
    return Ok(());
}
// Clean up the probes before building the real fixture.
fs::remove_file(&probe)?;
fs::remove_file(&probe_lower)?;
```

### Cross-platform gate (destination probe)

```text
// Decide which assertion bundle applies by re-running the probe on
// the destination.
let dest_probe = dest_dir.join("Probe");
let dest_probe_lower = dest_dir.join("probe");
File::create(&dest_probe).expect("dest probe upper");
let dest_case_sensitive = File::create(&dest_probe_lower).is_ok();
fs::remove_file(&dest_probe).ok();
fs::remove_file(&dest_probe_lower).ok();
```

### CI matrix coverage

The test runs unmodified on every CI lane:

- `linux-musl` / `linux-gnu` runners: case-sensitive source and
  destination; exercises the "no warning, both files" branch.
- `windows-2022` runner: NTFS destination; exercises the "warning,
  one file, second-write content wins" branch.
- `macos-14` runner: APFS default (case-insensitive) destination;
  exercises the same case-insensitive branch as Windows. The source
  probe gate skips the test if the source dir is also on a
  case-insensitive volume.

## 8. Findings

- **F1: No case-collision detection exists today.** Section 3's
  ripgrep sweep returns zero hits in `crates/transfer/src/`,
  `crates/engine/src/`, or `crates/metadata/src/` that compare a
  source basename against an existing destination entry by case-fold.
  No Win32 case-sensitivity API
  (`FILE_FLAG_POSIX_SEMANTICS`, `FileCaseSensitiveInformation`,
  `FSCTL_SET_CASE_SENSITIVE_INFO`) is referenced anywhere in the
  workspace. The remove-and-retry handlers at
  `crates/engine/src/local_copy/executor/file/guard.rs:325` and the
  symlink / fifo / device variants under
  `crates/engine/src/local_copy/executor/special/` consume
  `io::ErrorKind::AlreadyExists` without inspecting the conflicting
  inode's name casing.
- **F2: Behaviour today on Windows is strategy A.** The second write
  of a case-distinct pair opens the existing entry through the
  default `OBJ_CASE_INSENSITIVE` parse, truncates, and rewrites in
  place. The destination filename retains the case of the
  first-enumerated source file (`Makefile` in the test fixture). The
  destination contents reflect the second-written source file
  (`makefile`'s payload). No warning is emitted; the exit code is
  `Success` (`0`).
- **F3: Behaviour today on Linux destinations on case-insensitive
  mounts is strategy A.** A `vfat` / `exfat` / `ntfs-3g`-without-
  `windows_names` / case-insensitive-APFS / case-insensitive-HFS+
  destination dispatches the second `open(O_WRONLY|O_CREAT|O_TRUNC)`
  through the destination filesystem driver, which case-folds and
  reopens the first file's inode. Identical outcome to F2 - second
  payload wins, first casing wins, no warning, exit `Success`.
- **F4: No regression coverage for the scenario.** `ripgrep` for
  `case_collision`, `case_conflict`, or `Makefile.*makefile` across
  `crates/transfer/tests/`, `crates/engine/tests/`,
  `crates/metadata/tests/`, and `tests/` returns no result. The WPC-11
  follow-up landing the test from section 7 closes the coverage gap.
- **F5: Per-directory NTFS case-sensitive flag flips behaviour.** A
  destination directory with `FILE_CS_FLAG_CASE_SENSITIVE_DIR` set
  (via `fsutil.exe file setCaseSensitiveInfo <dir> enable` or
  `FSCTL_SET_CASE_SENSITIVE_INFO`) honours case at the kernel path-
  parse layer. Both source files then land as separate destination
  entries, matching the case-sensitive Linux outcome. oc-rsync neither
  detects nor sets this flag today; the recommended strategy (B)
  detection logic should be skipped (no warning, treat the destination
  as case-sensitive) when the parent directory's flag is set. The
  detection probe for the flag is
  `GetFileInformationByHandleEx(handle, FileCaseSensitiveInfo, &info,
  sizeof info)` against a handle opened on the destination's parent
  directory. The probe is cheap and can be cached per directory in the
  generator's recursion cursor.

## 9. Cross-references

- WPC-13 Windows support matrix - `docs/user/windows-support-matrix.md`
  (PR #4920). Lists WPC-11 under "Case-insensitive filesystem conflict
  detection".
- WPC-5 long-path support audit -
  `docs/audit/windows-long-path-support.md` (PR #4940). Adjacent
  NTFS-quirk audit; same `crates/fast_io/src/` and
  `crates/metadata/src/` call-site surface.
- WPC-7 reparse-point classification audit -
  `docs/audit/windows-reparse-point-classification.md` (PR #4937).
  Companion audit that also documents zero existing detection / one
  follow-up implementation task.
- Memory notes: [[project_windows_real_world_parity_unclear]],
  [[project_windows_parity_wip]].
