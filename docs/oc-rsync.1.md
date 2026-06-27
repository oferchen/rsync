---
title: OC-RSYNC
section: 1
header: User Commands
footer: oc-rsync 0.5.8
date: 2026-02-28
---

# NAME

oc-rsync - fast, wire-compatible rsync implementation in Rust

# SYNOPSIS

**oc-rsync** [*OPTION*]... *SOURCE*... *DEST*

**oc-rsync** [*OPTION*]... *SOURCE*... [*USER*@]*HOST*:*DEST*

**oc-rsync** [*OPTION*]... [*USER*@]*HOST*:*SOURCE*... *DEST*

**oc-rsync** [*OPTION*]... [*USER*@]*HOST*::*MODULE*[/*PATH*]... *DEST*

**oc-rsync** [*OPTION*]... rsync://[*USER*@]*HOST*[:*PORT*]/*MODULE*[/*PATH*]... *DEST*

**oc-rsync** **--daemon** [**--config**=*FILE*] [**--no-detach**]

# DESCRIPTION

**oc-rsync** is a Rust reimplementation of rsync that is wire-compatible with
upstream rsync 3.4.1 (protocol version 32). It works as a drop-in replacement
for the traditional C rsync binary, supporting local copies, remote transfers
over SSH, and daemon-mode connections over the rsync protocol.

**oc-rsync** uses the same delta-transfer algorithm as upstream rsync to
minimize data sent over the network. Only the differences between source and
destination files are transmitted, making it efficient for synchronizing large
directory trees over slow links.

When a single colon separates a host specification from a path, the transfer
uses a remote shell (SSH by default). When double colons or an rsync:// URL
are used, the transfer connects directly to an rsync daemon on port 873.

The binary name **oc-rsync** allows it to be installed alongside the system
rsync without conflict.

## Protocol Compatibility

**oc-rsync** supports rsync protocol versions 28 through 32 and has been
validated against upstream rsync versions 3.0.9, 3.1.3, 3.4.1, and 3.4.2.
Incremental recursion, checksum negotiation (including XXH3 and XXH128),
and all standard wire-format features are supported.

The CI interop matrix covers the following upstream rsync version, protocol,
and transfer-mode combinations:

| upstream rsync | protocol | mode (push/pull/daemon)  | status (CI-verified) |
|----------------|----------|--------------------------|----------------------|
| 2.6.9          | 29       | push (daemon)            | non-blocking (RP28.c) |
| 2.6.9          | 29       | pull (daemon)            | non-blocking (RP28.d) |
| 3.0.9          | 30       | push, pull, daemon       | gating |
| 3.1.3          | 31       | push, pull, daemon       | gating |
| 3.4.1          | 32       | push, pull, daemon, SSH  | gating |
| 3.4.2          | 32       | push, pull, daemon       | gating |

Wire format is verified byte-identical to upstream rsync via CI golden-byte
tests for the listed versions. Other versions may work but are not
regression-tested.

## Performance

**oc-rsync** uses a threaded architecture instead of the fork-based pipeline
used by upstream rsync. For local transfers, this reduces syscall overhead
and context switches. SIMD-accelerated checksum implementations (AVX2, SSE2,
NEON) are used where available, with automatic scalar fallbacks.

# OPTIONS

## General

**--help**
:   Show help message and exit.

**-V**, **--version**
:   Output version information and exit.

**-v**, **--verbose**
:   Increase verbosity. Can be repeated for more detail (up to **-vvvv**).

**-q**, **--quiet**
:   Suppress non-error messages.

**-n**, **--dry-run**
:   Perform a trial run without making changes. Shows what would be
    transferred without actually copying files or modifying the destination.

**--list-only**
:   List files without performing a transfer.

**-h**, **--human-readable**
:   Output numbers in a human-readable format. Can be repeated: **-h** enables
    suffixed values (1.23K, 4.56M); **-hh** shows both human-readable and
    exact values. Optionally accepts **=***LEVEL* (0, 1, or 2).

**--no-human-readable**
:   Disable human-readable number formatting.

**-8**, **--8-bit-output**
:   Leave high-bit characters unescaped in output.

**-i**, **--itemize-changes**
:   Output a change summary for each updated entry.

**--out-format**=*FORMAT*
:   Customize transfer output using *FORMAT* for each processed entry.
    Also available as **--log-format**.

**--stats**
:   Output transfer statistics after completion.

**--progress**
:   Show progress information during transfers.

**--no-progress**
:   Disable progress reporting.

**-P**
:   Equivalent to **--partial** **--progress**.

**--info**=*FLAGS*
:   Fine-grained control over informational output. Use **--info=help** for
    available flag names.

**--debug**=*FLAGS*
:   Fine-grained control over debug output. Use **--debug=help** for
    available flag names.

**--msgs2stderr**
:   Send informational messages to standard error instead of standard output.

**--no-msgs2stderr**
:   Send messages to standard output (default).

**--stderr**=*MODE*
:   Change stderr output mode. *MODE* may be **errors**, **all**, or **client**.

**--outbuf**=*MODE*
:   Set stdout buffering to *MODE*: **N** (none), **L** (line), or **B** (block).

**--max-alloc**=*SIZE*
:   Limit memory allocation to *SIZE* bytes. Supports suffixes: K, M, G.

**--log-file**=*FILE*
:   Write per-file transfer information to *FILE*.

**--log-file-format**=*FORMAT*
:   Customize the format used when appending to **--log-file**.

## Transfer Options

**-a**, **--archive**
:   Archive mode. This is a shorthand that enables **-rlptgoD** (recursive,
    links, permissions, times, group, owner, devices, and specials).

**-r**, **--recursive**
:   Recurse into directories when processing source operands. Implied by
    **--archive**.

**--no-recursive**
:   Do not recurse into directories.

**--inc-recursive**
:   Scan directories incrementally during recursion (default behavior). This
    allows transfers to begin before the entire file list is built.

**--no-inc-recursive**
:   Disable incremental directory scanning during recursion. The entire
    file list is built before transferring begins.

**-d**, **--dirs**
:   Copy directory entries even when recursion is disabled.

**--no-dirs**
:   Skip directory entries when recursion is disabled.

**-l**, **--links**
:   Copy symlinks as symlinks. Implied by **--archive**.

**--no-links**
:   Do not copy symlinks as symlinks.

**-L**, **--copy-links**
:   Transform symlinks into referent files or directories (follow symlinks).

**--copy-unsafe-links**
:   Transform unsafe symlinks (those pointing outside the transfer tree)
    into referent files or directories.

**--safe-links**
:   Skip symlinks that point outside the transfer root.

**-k**, **--copy-dirlinks**
:   Transform symlinked directories into referent directories.

**-K**, **--keep-dirlinks**
:   Treat existing destination symlinks to directories as directories.

**--munge-links**
:   Munge symlinks to make them safe in daemon mode.

**--no-munge-links**
:   Disable symlink munging.

**-H**, **--hard-links**
:   Preserve hard links between files.

**--no-hard-links**
:   Disable hard link preservation.

**-p**, **--perms**
:   Preserve file permissions. Implied by **--archive**.

**--no-perms**
:   Disable permission preservation.

**-E**, **--executability**
:   Preserve executability without altering other permission bits.

**--chmod**=*SPEC*
:   Apply chmod-style *SPEC* modifiers to received files. Can be specified
    multiple times.

**-o**, **--owner**
:   Preserve file ownership. Requires appropriate privileges. Implied by
    **--archive**.

**--no-owner**
:   Disable ownership preservation.

**-g**, **--group**
:   Preserve file group. Requires suitable privileges. Implied by
    **--archive**.

**--no-group**
:   Disable group preservation.

**--chown**=*USER*:*GROUP*
:   Set destination ownership to *USER* and/or *GROUP*.

**--copy-as**=*USER*[:*GROUP*]
:   Run receiver with specified *USER* and optional *GROUP* for privileged copy.

**--usermap**=*STRING*
:   Apply custom user ID mapping rules (*SRCUSER*:*DESTUSER*,...).

**--groupmap**=*STRING*
:   Apply custom group ID mapping rules (*SRCGROUP*:*DESTGROUP*,...).

**--numeric-ids**
:   Preserve numeric UID/GID values without name mapping.

**--no-numeric-ids**
:   Map UID/GID values to names when possible (default).

**-t**, **--times**
:   Preserve modification times. Implied by **--archive**.

**--no-times**
:   Disable modification time preservation.

**-O**, **--omit-dir-times**
:   Skip preserving directory modification times.

**--no-omit-dir-times**
:   Preserve directory modification times.

**-J**, **--omit-link-times**
:   Skip preserving symlink modification times.

**--no-omit-link-times**
:   Preserve symlink modification times.

**-U**, **--atimes**
:   Preserve access times.

**--no-atimes**
:   Disable access time preservation.

**-N**, **--crtimes**
:   Preserve creation times (macOS/Windows).

**--no-crtimes**
:   Disable creation time preservation.

**-A**, **--acls**
:   Preserve POSIX ACLs on Linux, macOS, and FreeBSD via `exacl`.

    For receiver-side UID/GID handling when an ACL entry names a principal
    that does not resolve locally, see `docs/user-guide/acl-id-mapping.md`.
    Since PR #4742 (commit `07f81641f`), unmappable named entries are
    preserved with their raw wire id rather than silently dropped, matching
    upstream rsync 3.4.2 `acls.c::recv_ida_entries`.

    On Windows, **--acls** preserves the NTFS discretionary ACL (DACL) via
    `GetNamedSecurityInfoW`/`SetNamedSecurityInfoW`. The Windows path is a
    Tier 1C partial implementation: it interoperates with upstream rsync
    and POSIX peers through the standard cross-platform ACL wire format,
    but it cannot represent every NTFS construct losslessly. The following
    losses are documented and emit a one-time warning per transfer:

      - **Deny ACEs are dropped.** POSIX has no deny equivalent, so explicit
        `ACCESS_DENIED_ACE_TYPE` entries are silently omitted from the wire
        payload.
      - **Inherited ACEs are not transmitted.** ACEs with the
        `INHERITED_ACE` flag are skipped; the receiver relies on the
        destination's own inheritance chain. Mirrors how POSIX default ACLs
        are handled as a separate stream.
      - **The system ACL (SACL) is skipped** unless the planned
        **--audit-acls** flag is passed and the sender holds
        `SE_SECURITY_NAME`. Audit ACEs cannot ride the cross-platform
        payload.
      - **Non-`rwx` access bits collapse.** `FILE_GENERIC_READ`/`WRITE`/`EXECUTE`
        map cleanly to `r`/`w`/`x`. Bits outside that triplet (`DELETE`,
        `WRITE_DAC`, `WRITE_OWNER`, `SYNCHRONIZE`, generic bits) are
        dropped on send and re-synthesised as `FILE_GENERIC_*` plus
        `SYNCHRONIZE` on receive.
      - **Unresolvable SIDs are dropped.** Trustee SIDs that
        `LookupAccountSidW` cannot translate to an account name are omitted
        from the cross-platform payload; on receive, names that
        `LookupAccountNameW` cannot resolve cause the ACE to be dropped
        with a warning.

    The cross-platform payload is the standard upstream byte stream
    (varint count, per-entry tag, permission triplet, optional name). A
    second, opt-in payload encoding the full NTFS security descriptor as
    SDDL on the existing xattr stream under
    `user.win32.security_descriptor` is planned (see
    **--windows-acls** below). SDDL is the textual form produced and
    consumed by `ConvertSecurityDescriptorToStringSecurityDescriptorW`;
    Windows-to-Windows transfers may use it for higher fidelity while
    remaining backward-compatible with peers that only understand the
    cross-platform payload.

    See `docs/design/windows-ntfs-acl-support.md` for the full mapping
    matrix, hardlink handling, and the implementation roadmap.

    ### ACL ID-remap interop matrix

    Coverage status for the receiver-side ACL ID-remap path introduced by
    ACL-1 (PR #4742, commit `07f81641f`), validated by the interop tests
    in `crates/metadata/tests/acl_root_root_interop.rs`. Each cell names
    the directional pair, the receiver privilege level, the ID
    mappability case, and the test that exercises it.

    | Direction              | Receiver | IDs (mappable / unmappable)    | Status | Test                                                 |
    |------------------------|----------|--------------------------------|--------|------------------------------------------------------|
    | oc sender, upstream rx | root     | mappable (root/root)           | green  | `root_to_root_oc_then_upstream_preserves_acls_byte_identically` (ACL-2.a) |
    | upstream sender, oc rx | root     | mappable (root/root)           | green  | `root_to_root_upstream_then_oc_preserves_acls_byte_identically` (ACL-2.a) |
    | oc sender, oc rx       | non-root | unmappable UID (1500000)       | green  | `non_root_sender_unmappable_uid_remap_matches_upstream` (ACL-2.b)         |
    | oc sender, oc rx       | non-root | unmappable GID (1500001)       | green  | `non_root_sender_unmappable_gid_remap_matches_upstream` (ACL-2.b)         |
    | upstream sender, oc rx | non-root | mixed (root + 1500000/1500001) | green  | `acl_2c_upstream_sender_oc_receiver` (ACL-2.c)                            |

    All five cells are wire-validated against upstream rsync 3.4.x: each
    test snapshots the source ACL, runs both oc-rsync and upstream as the
    counter-party, and asserts the receiver's on-disk ACL is byte-
    identical to the source AND to upstream's own output for the same
    fixture. Mappable named entries route through `getpwnam_r` /
    `getgrnam_r` and install the resolved local id; unmappable entries
    round-trip verbatim per upstream `uidlist.c:282` `id2 = id`. The
    underlying fix is ACL-1 (`crates/metadata/src/acl_exacl/error.rs`
    `is_unsupported_error` plus `crates/metadata/src/acl_exacl/reconstruct.rs`
    `resolve_ida_id`); see `docs/user-guide/acl-id-mapping.md` for the
    user-facing semantics.

**--no-acls**
:   Disable ACL preservation.

**--audit-acls** (planned)
:   On Windows, request that the sender read the system ACL (SACL) in
    addition to the DACL. Requires the `SE_SECURITY_NAME` privilege; the
    sender skips the SACL with a warning when the privilege probe fails.
    SACL contents are carried only in the SDDL fidelity payload (see
    **--windows-acls**), never in the cross-platform payload. Not yet
    wired; see `docs/design/windows-ntfs-acl-support.md` section 4.2.

**--fail-on-windows-acl-loss** (planned)
:   On Windows, abort the transfer with exit code 23 (partial transfer)
    when an NTFS DACL contains deny ACEs, audit ACEs, inherited ACEs that
    cannot be expressed in POSIX, or owner/group SIDs the receiver cannot
    resolve. Use this when a transfer must either preserve the source
    DACL verbatim or fail loudly rather than silently lower to POSIX
    rwx. Not yet wired; see `docs/design/windows-ntfs-acl-support.md`
    section 4.3.

**--windows-acls** (planned)
:   On Windows, opt in to the Windows-to-Windows SDDL fidelity payload in
    addition to the cross-platform payload. When both peers advertise the
    `W` capability bit, the sender writes the full NTFS security
    descriptor as SDDL into the `user.win32.security_descriptor` xattr
    via `ConvertSecurityDescriptorToStringSecurityDescriptorW`, and the
    receiver reconstructs the descriptor verbatim via
    `ConvertStringSecurityDescriptorToSecurityDescriptorW`. Falls back to
    the cross-platform payload when either peer does not advertise `W`.
    Not yet wired; see `docs/design/windows-ntfs-acl-support.md` section
    4.2.

**-X**, **--xattrs**
:   Preserve extended attributes when supported (Unix only).

**--no-xattrs**
:   Disable extended attribute preservation.

**-D**
:   Preserve device and special files. Equivalent to **--devices** **--specials**.
    Implied by **--archive**.

**--devices**
:   Preserve device files (block and character special files).

**--no-devices**
:   Disable device file preservation.

**--copy-devices**
:   Copy device file contents as regular files.

**--write-devices**
:   Write file data directly to device files instead of creating nodes.

**--no-write-devices**
:   Do not write file data directly to device files.

**--specials**
:   Preserve special files (sockets, FIFOs).

**--no-specials**
:   Disable preservation of special files.

**--super**
:   Receiver attempts super-user activities (implies **--owner**, **--group**,
    and **--perms**).

**--no-super**
:   Disable super-user handling even when running as root.

**--fake-super**
:   Store/restore privileged attributes using extended attributes instead of
    real permissions.

**--no-fake-super**
:   Disable fake-super mode.

**-S**, **--sparse**
:   Handle sparse files efficiently. Attempts to create sparse files on the
    destination when appropriate.

**--no-sparse**
:   Disable sparse file handling.

**-W**, **--whole-file**
:   Copy files without using the delta-transfer algorithm. Disables the
    rsync algorithm and transfers whole files instead.

**--no-whole-file**
:   Enable the delta-transfer algorithm (disable whole-file copies).

**--inplace**
:   Write updated data directly to destination files instead of using
    temporary files. More efficient but less safe if interrupted.

**--no-inplace**
:   Use temporary files when updating regular files (default).

**--append**
:   Append data to existing destination files without rewriting preserved bytes.

**--no-append**
:   Disable append mode for destination updates.

**--append-verify**
:   Append data while verifying that existing bytes match the sender.

**-c**, **--checksum**
:   Skip files based on checksum rather than mod-time and size. Forces full
    content comparison using checksums.

**--no-checksum**
:   Disable checksum-based change detection (use quick check).

**--size-only**
:   Skip files whose size already matches the destination, ignoring
    timestamps.

**-I**, **--ignore-times**
:   Disable quick checks based on size and modification time. Treat all
    files as changed.

**--ignore-existing**
:   Skip updating files that already exist at the destination.

**--existing**
:   Skip creating new files that do not already exist at the destination.
    Also available as **--ignore-non-existing**.

**-u**, **--update**
:   Skip files that are newer on the destination.

**--modify-window**=*SECS*
:   Treat timestamps differing by less than *SECS* seconds as equal. Useful
    for FAT filesystems where timestamps have 2-second resolution.

**--ignore-missing-args**
:   Skip missing source arguments without reporting an error.

**--delete-missing-args**
:   Remove destination entries when their source argument is missing.

**-R**, **--relative**
:   Preserve source path components relative to the current directory.

**--no-relative**
:   Disable preservation of source path components.

**-x**, **--one-file-system**
:   Do not cross filesystem boundaries during traversal. Specify twice
    (**-xx**) to also skip root-level mount points.

**--no-one-file-system**
:   Allow traversal across filesystem boundaries.

**--implied-dirs**
:   Create parent directories implied by source paths.

**--no-implied-dirs**
:   Disable creation of parent directories implied by source paths.

**--mkpath**
:   Create destination's missing path components.

**--no-mkpath**
:   Disable creation of destination path components. Also available as
    **--old-dirs**.

**-m**, **--prune-empty-dirs**
:   Skip creating directories that remain empty after filters.

**--no-prune-empty-dirs**
:   Disable pruning of empty directories.

**--force**
:   Remove conflicting destination directories to make way for files.

**--no-force**
:   Preserve conflicting destination directories.

**-y**, **--fuzzy**
:   Find similar files at the destination to use as a basis for
    delta transfers.

**--no-fuzzy**
:   Disable fuzzy basis file search.

**--trust-sender**
:   Trust the sender's file list without additional verification.

**-b**, **--backup**
:   Create backups before overwriting or deleting existing entries.

**--no-backup**
:   Disable backup creation.

**--backup-dir**=*DIR*
:   Store backups inside *DIR* instead of alongside the destination.

**--suffix**=*SUFFIX*
:   Append *SUFFIX* to backup names (default **~**).

**--remove-source-files**
:   Remove source files after a successful transfer. Also available as
    **--remove-sent-files**.

**--partial**
:   Keep partially transferred files when a transfer is interrupted (signal,
    connection drop, or error). Without this option, **oc-rsync** deletes the
    temporary file for any incomplete transfer, leaving the destination
    unchanged.

    When **--partial** is active, the incomplete temporary file is renamed to
    the final destination path, replacing any existing file. On Unix the
    retained file's modification time is set to the epoch (1970-01-01
    00:00:00 UTC) so that a subsequent **--update** run will not skip it -
    the epoch mtime is always older than any real source file. On Windows
    (NTFS), the epoch mtime cannot be represented; the file keeps whatever
    mtime it had at the time of interruption.

    On a subsequent run, **oc-rsync** detects that the destination file
    differs from the source (by size and mtime) and uses it as a basis for
    delta transfer, resuming where the previous transfer left off.

**--no-partial**
:   Discard partially transferred files on error. This is the default
    behavior: the temporary file is removed and the destination is left
    unchanged.

**--partial-dir**=*DIR*
:   Store partially transferred files in *DIR* instead of at the final
    destination path. Implies **--partial**.

    When a transfer is interrupted, the incomplete temporary file is moved
    into *DIR* (using the destination's relative path as the filename within
    *DIR*). On a subsequent run, the receiver checks *DIR* for a matching
    basis file and uses it for delta transfer resumption.

    This avoids leaving incomplete files at their final destination, which
    is useful when other processes read from the destination tree.

**-T**, **--temp-dir**=*DIR*
:   Store temporary files in *DIR* while transferring. Also available as
    **--tmp-dir**.

**--delay-updates**
:   Accumulate all updated files in a staging area and rename them to their
    final destinations only after the entire transfer completes
    successfully. This provides atomic updates at the cost of additional
    disk space.

    When no explicit **--partial-dir** is configured, **--delay-updates**
    implicitly sets **--partial-dir** to **.~tmp~** and enables
    **--partial**. Files are written into the staging directory during the
    transfer and moved to their final paths in a single rename sweep at the
    end.

    If the transfer is interrupted before the final rename sweep, all files
    remain in the staging directory (**.~tmp~** or the configured
    **--partial-dir**). The destination tree is untouched. A subsequent run
    resumes from the staged files.

**--no-delay-updates**
:   Write updated files immediately during the transfer.

**--preallocate**
:   Pre-allocate disk space for destination files before writing.

**--fsync**
:   Call fsync() to ensure data is written to stable storage after writing.

**--open-noatime**
:   Attempt to open source files without updating access times (O_NOATIME).

**--no-open-noatime**
:   Disable O_NOATIME handling.

## Delete Options

**--delete**
:   Remove destination files that are absent from the source. Implies
    **--delete-during** by default.

**--delete-before**
:   Remove extraneous destination files before transfers start.

**--delete-during**
:   Remove extraneous destination files during directory traversal. Also
    available as **--del**.

**--delete-delay**
:   Compute deletions during the transfer and apply them after the run
    completes.

**--delete-after**
:   Remove extraneous destination files after transfers complete.

**--delete-excluded**
:   Remove excluded destination files during deletion.

**--max-delete**=*NUM*
:   Limit the number of deletions that may occur per run.

**--ignore-errors**
:   Continue deleting files even when there are I/O errors.

**--no-ignore-errors**
:   Stop deleting if I/O errors occur (default).

## Filter Options

**--filter**=*RULE*, **-f** *RULE*
:   Apply filter *RULE*. Supports **+** (include), **-** (exclude), **!**
    (clear), **protect** *PATTERN*, **risk** *PATTERN*, **merge**[,*MODS*]
    *FILE* (or **.** [,*MODS*] *FILE*), and **dir-merge**[,*MODS*] *FILE*
    (or **:** [,*MODS*] *FILE*).

**--exclude**=*PATTERN*
:   Skip files matching *PATTERN*.

**--exclude-from**=*FILE*
:   Read exclude patterns from *FILE*, one per line.

**--include**=*PATTERN*
:   Re-include files matching *PATTERN* after exclusions.

**--include-from**=*FILE*
:   Read include patterns from *FILE*, one per line.

**-C**, **--cvs-exclude**
:   Auto-ignore files using CVS-style ignore rules.

**--apple-double-skip**
:   Skip macOS AppleDouble (`._foo`) sidecar files. macOS writes these files on
    filesystems that cannot represent extended attributes natively (FAT, exFAT,
    most network shares) to carry FinderInfo, resource forks, and xattrs.
    Replicating them onto other systems usually clutters destinations with
    stale metadata; enabling this flag appends `._*` to the filter chain as a
    perishable exclusion so explicit include rules supplied earlier still win.

**-F**
:   Shortcut for per-directory .rsync-filter handling. Repeat (**-FF**) to
    also load receiver-side filter files.

**--files-from**=*FILE*
:   Read additional source operands from *FILE*.

**--from0**, **-0**
:   Treat file list entries as NUL-terminated records.

**--no-from0**
:   Disable NUL-terminated file list handling.

## Size Limits

**--min-size**=*SIZE*
:   Skip files smaller than *SIZE*. Supports suffixes: K, M, G.

**--max-size**=*SIZE*
:   Skip files larger than *SIZE*. Supports suffixes: K, M, G.

## Delta Transfer Options

**-z**, **--compress**
:   Compress file data during transfers. Do not combine with SSH stream
    compression (**ssh -C** or **Compression yes** in **ssh_config**); see
    *Avoiding double-compression over SSH* under **NOTES**.

**--no-compress**
:   Disable compression.

**--compress-level**=*NUM*
:   Set compression level (0-9). Level 0 disables compression; level 9
    provides maximum compression. Also available as **--zl**.

**--compress-choice**=*ALGO*
:   Select compression algorithm. Valid values: **zlib**, **zstd**, **lz4**.
    Also available as **--zc**.

**--compress-threads**=*N*
:   Set the number of worker threads zstd uses internally. A value of **0**
    (the default) lets zstd choose. Positive values up to **64** are accepted.
    Also available as **--zt**.

**--old-compress**
:   Use old-style (zlib) compression.

**--new-compress**
:   Use new-style compression (typically zstd).

**--skip-compress**=*LIST*
:   Skip compressing files with suffixes in *LIST*.

**--checksum-choice**=*ALGO*
:   Select the strong checksum algorithm. Valid values: **auto**, **none**,
    **md4**, **md5**, **xxh64**, **xxh3**, **xxh128**. Also available as
    **--cc**. **none** disables the transfer checksum and forces
    **--whole-file** (mirrors upstream `checksum.c:197-198`).

**--checksum-seed**=*NUM*
:   Set the checksum seed for xxhash-based algorithms.

**--block-size**=*SIZE*
:   Force the delta-transfer block size to *SIZE* bytes. Larger blocks
    reduce overhead but may miss small changes.

## Comparison Directories

**--compare-dest**=*DIR*
:   Skip creating destination files that match files in *DIR*.

**--copy-dest**=*DIR*
:   Copy matching files from *DIR* instead of transferring from the source.

**--link-dest**=*DIR*
:   Hard-link matching files from *DIR* into the destination.

## Network Options

**-e**, **--rsh**=*COMMAND*
:   Use remote shell *COMMAND* for remote transfers (default: ssh).

**--rsync-path**=*PROGRAM*
:   Use *PROGRAM* as the remote rsync executable.

**--connect-program**=*COMMAND*
:   Execute *COMMAND* to reach rsync:// daemons. Supports **%H** (hostname)
    and **%P** (port) placeholders.

**--jump-host**=*[user@]HOST[:PORT][,...]*
:   Comma-separated proxy-jump hosts. Forwarded to the remote shell as
    **ssh -J** *value* when the configured remote shell is OpenSSH. Only
    the long form is provided; the short flag **-J** is reserved by upstream
    rsync for **--omit-link-times**.

**-M**, **--remote-option**=*OPTION*
:   Forward *OPTION* to the remote rsync command. Can be specified multiple
    times.

**-s**, **--protect-args**
:   Protect remote shell arguments from expansion. Also available as
    **--secluded-args**.

**--no-protect-args**
:   Allow the remote shell to expand wildcard arguments.

**--old-args**
:   Use old-style argument handling (pre-rsync 3.2.4 behavior).

**--no-old-args**
:   Use new-style argument handling (default).

**-4**, **--ipv4**
:   Prefer IPv4 when connecting to remote hosts.

**-6**, **--ipv6**
:   Prefer IPv6 when connecting to remote hosts.

**--address**=*ADDRESS*
:   Bind outgoing connections to *ADDRESS*.

**--port**=*PORT*
:   Use *PORT* as the default rsync daemon TCP port (default: 873).

**--sockopts**=*OPTIONS*
:   Set additional socket options (comma-separated list).

**--blocking-io**
:   Force the remote shell to use blocking I/O.

**--no-blocking-io**
:   Disable forced blocking I/O on the remote shell.

**--timeout**=*SECS*
:   Set I/O timeout in seconds. Abort when no data is received for *SECS*
    seconds. 0 disables the timeout.

**--no-timeout**
:   Disable I/O timeout.

**--contimeout**=*SECS*
:   Set connection timeout in seconds. 0 disables the limit.

**--no-contimeout**
:   Disable connection timeout.

**--protocol**=*NUM*
:   Force protocol version *NUM* when accessing rsync daemons. Valid range:
    28-32.

**--bwlimit**=*RATE*[:*BURST*]
:   Limit I/O bandwidth in KiB/s. Supports decimal, binary, and IEC unit
    suffixes. Optional *:BURST* caps the token bucket. 0 disables the limit.

**--no-bwlimit**
:   Remove any configured bandwidth limit.

**--iconv**=*CONVERT_SPEC*
:   Convert filenames using iconv. Use **.** for locale defaults or specify
    *LOCAL*,*REMOTE* charsets.

**--no-iconv**
:   Disable iconv charset conversion.

**--aes**
:   Force AES-GCM ciphers for SSH connections. Provides hardware-accelerated
    encryption on CPUs with AES-NI or ARMv8 crypto extensions.

**--ssl**
:   Connect to an rsync daemon over TLS instead of cleartext. Changes the
    default port from 873 to 874. The server certificate is verified against
    the Mozilla root CA bundle; use **--ssl-ca-cert** to specify a custom CA.
    Requires the **client-tls** feature at build time.

**--ssl-ca-cert**=*FILE*
:   Use *FILE* as the PEM-encoded CA bundle for server certificate
    verification when connecting with **--ssl**. Overrides the built-in
    Mozilla root CAs. Useful with private CAs or self-signed certificates.

**--password-file**=*FILE*
:   Read daemon passwords from *FILE* when contacting rsync:// daemons.

**--no-motd**
:   Suppress daemon message-of-the-day lines.

## Batch Options

**--write-batch**=*PREFIX*
:   Store updated data in batch files named *PREFIX* for later replay.
    Compression (**--compress**) is not supported with batch mode at protocol 28
    (rsync 2.x servers) because the zlib streaming state cannot be serialized
    into the batch file at that protocol version.

**--only-write-batch**=*PREFIX*
:   Write batch files named *PREFIX* without applying the updates locally.

**--read-batch**=*PREFIX*
:   Apply updates stored in batch files named *PREFIX*.

**--early-input**=*FILE*
:   Read *FILE* early in the transfer (before file list exchange).

## Daemon Options

**--daemon**
:   Run as an rsync daemon, serving files to rsync clients.

**--config**=*FILE*
:   Specify alternate daemon configuration file. Default:
    */etc/oc-rsyncd/oc-rsyncd.conf*.

**--detach**
:   Detach from the terminal and run as a background daemon.

**--no-detach**
:   Do not detach from the terminal (run daemon in foreground).

**--dparam**=*OVERRIDE*
:   Override daemon config parameter on the command line. Can be specified
    multiple times.

**--max-connections**=*N*
:   Cap the number of concurrent client connections the daemon will accept.
    *N* must be a positive integer. When omitted the daemon imposes no
    admission cap, so operating-system limits (file descriptors, RAM) are the
    only ceiling. When the cap is reached the accept loop refuses each new
    socket with the upstream-compatible greeting
    `@ERROR: max connections (N) reached -- try again later` and closes it
    without dispatching a session; the listener keeps running and serving
    in-flight clients. Mirrors the per-module `max connections` directive
    documented in **rsyncd.conf**(5); this flag enforces the same cap
    daemon-wide from the command line. **NOTE:** publicly exposed daemons
    SHOULD set this flag (or the equivalent `max connections` directive in
    *oc-rsyncd.conf*). Releases prior to **v0.6.2** have no admission cap
    and accept connections until the operating system runs out of resources.

**--max-sessions**=*N*
:   Cap the total number of sessions the daemon will serve before exiting.
    *N* must be a positive integer. After serving *N* sessions the daemon
    stops accepting new connections and exits once in-flight transfers
    complete. Useful for periodic restart under a process supervisor.
    Distinct from **--max-connections**, which caps *concurrent* sessions
    without bounding the lifetime total. Pass **--once** as a shorthand for
    **--max-sessions**=*1*.

## Performance Options

The io_uring policy selects one of three modes for the asynchronous file I/O
backend on Linux. Specify at most one of **--io-uring** or **--no-io-uring**;
omitting both leaves the policy in its default *auto* state.

| Policy | Selected by | Behaviour |
|--------|-------------|-----------|
| *auto* (default) | neither flag | Probe the kernel at startup. Use io_uring when available (Linux 5.6+), otherwise fall back to standard buffered I/O without warning. |
| *enabled* | **--io-uring** | Require io_uring. Exit with an error at startup if probing fails (kernel < 5.6, **io_uring_setup**(2) blocked by seccomp, or feature compiled out). |
| *disabled* | **--no-io-uring** | Skip the kernel probe and always use standard buffered I/O even when io_uring is available. Useful for benchmarking or when running under restrictive sandboxes. |

The probe additionally selects between standard submission and SQPOLL mode.
SQPOLL requires **CAP_SYS_NICE**; when that capability is missing the runtime
falls back to standard submission and emits a one-time log message. The active
backend is reported by **--version** and by **-vv** output, where the label is
*standard I/O*, *io_uring*, or *io_uring (SQPOLL)*.

On non-Linux targets and on builds without the *io_uring* feature compiled in,
*auto* and *disabled* both resolve to standard buffered I/O, while *enabled*
exits with an error.

**--io-uring**
:   Force io_uring for file I/O (policy *enabled*). Returns an error if
    io_uring is unavailable, including on non-Linux platforms, on Linux
    kernels older than 5.6, or when **io_uring_setup**(2) is blocked.
    Overrides a previous **--no-io-uring** on the same command line.

**--no-io-uring**
:   Disable io_uring (policy *disabled*). Always use standard buffered I/O
    even when the kernel supports io_uring. Overrides a previous
    **--io-uring** on the same command line.

**--zero-copy**
:   Allow I/O-level zero-copy primitives (**sendfile**(2), **splice**(2),
    **copy_file_range**(2), and io_uring **SEND_ZC**) when supported by the
    kernel. This is the default (policy *auto*/*enabled*).

    **NOTE:** the io_uring **SEND_ZC** dispatch is gated behind the
    `iouring-send-zc` cargo feature, which is **not** in the default feature
    set; the gate is documented as "Disabled by default pending
    kernel/workload benchmarks" in **crates/fast_io/Cargo.toml**. Default
    distro builds therefore use plain io_uring **SEND** even when
    **--zero-copy** is set; the other zero-copy primitives still apply where
    the kernel supports them. To get **SEND_ZC** dispatch, build with
    `cargo build --features iouring-send-zc` (requires Linux 5.16+). See
    **docs/design/iouring-send-zc.md** for the full rationale.

**--no-zero-copy**
:   Disable I/O-level zero-copy and route data through portable userspace
    read/write loops. Does not affect filesystem-level reflink / CoW cloning
    (see **--cow** / **--no-cow**).

The next two flags govern the receiver-side `SpillPolicy` that bounds the
concurrent-delta `ReorderBuffer`'s memory footprint. Both are *planned* for
STN-11 and will land in a future release; until then operators tune the same
knobs via the `OC_RSYNC_SPILL_DIR` and `OC_RSYNC_SPILL_THRESHOLD_BYTES`
environment variables (see *Receiver spill tunability* in the operator
migration guide). When the flags ship, they take precedence over the env
vars on the same invocation. The remaining `SpillPolicy` fields
(reclaim mode, granularity, compression) stay env-only by design; see
**docs/design/spill-policy-public-api.md** for the full surface.

**--spill-dir**=*PATH* (planned, STN-11)
:   Directory backing the receiver spill file. Created on first spill via
    `create_dir_all`. When unset the runtime defers to
    **std::env::temp_dir**(3) through a spooled tempfile that stays in
    memory up to 1 MiB before rolling over. Maps to `SpillPolicy.dir`.

**--spill-threshold-bytes**=*N*[**K**|**M**|**G**] (planned, STN-11)
:   Memory budget for the receiver reorder buffer before items spill to
    disk. Suffixes are case-insensitive base-1024 (`K`=KiB, `M`=MiB,
    `G`=GiB). Omitting the flag (or passing an empty value) leaves spill
    disabled and the consumer stays on the bare `ReorderBuffer` path; the
    value `0` is rejected. Maps to `SpillPolicy.threshold_bytes`.

## Scheduling Options

**--stop-after**=*MINS*
:   Stop the transfer after running for the specified number of minutes.
    Also available as **--time-limit**.

**--stop-at**=*WHEN*
:   Stop the transfer at the specified local time (e.g., HH:MM or
    YYYY-MM-DDTHH:MM).

## Internal Options

The following options are used internally by oc-rsync for remote invocation
and are not intended for direct use:

**--server**
:   Run in server mode (set automatically on the remote side of a transfer).

**--sender**
:   Mark this process as the sender role (used with **--server**).

# EXIT CODES

**0**
:   Success.

**1**
:   Syntax or usage error.

**2**
:   Protocol incompatibility.

**3**
:   Errors selecting input/output files, dirs.

**4**
:   Requested action not supported.

**5**
:   Error starting client-server protocol.

**6**
:   Daemon unable to append to log-file.

**10**
:   Error in socket I/O.

**11**
:   Error in file I/O.

**12**
:   Error in rsync protocol data stream.

**13**
:   Errors with program diagnostics.

**14**
:   Error in IPC code.

**15**
:   Received SIGSEGV, SIGBUS, or SIGABRT.

**16**
:   Received SIGINT, SIGTERM, or SIGHUP (terminated).

**19**
:   Received SIGUSR1.

**20**
:   Received SIGINT, SIGTERM, or SIGHUP (signal).

**21**
:   Some error returned by waitpid().

**22**
:   Error allocating core memory buffers.

**23**
:   Partial transfer due to error.

**24**
:   Some files vanished before they could be transferred.

**25**
:   The --max-delete limit stopped deletions.

**30**
:   Timeout in data send/receive.

**35**
:   Timeout waiting for daemon connection.

**124**
:   Remote command failed.

**125**
:   Remote command killed.

**126**
:   Remote command could not be run.

**127**
:   Remote command not found.

# ENVIRONMENT

**RSYNC_RSH**
:   Specifies the remote shell to use. Equivalent to **--rsh**. If both
    **RSYNC_RSH** and **--rsh** are set, the command-line option takes
    precedence.

**RSYNC_PASSWORD**
:   Provides the password for rsync daemon authentication. When set, the
    user is not prompted for a password when connecting to daemon-mode
    servers.

**RSYNC_PROTECT_ARGS**
:   Controls argument protection by default. Set to **1**, **yes**, **true**,
    or **on** to enable; set to **0**, **no**, **false**, or **off** to
    disable. Overridden by **--protect-args** or **--no-protect-args** on
    the command line.

**RSYNC_PROXY**
:   Specifies an HTTP proxy for rsync daemon connections, in
    *HOST*:*PORT* format.

**RSYNC_CONNECT_PROG**
:   Program to execute for establishing daemon connections. Supports
    **%H** (hostname) and **%P** (port) placeholders.

**OC_RSYNC_CONFIG**
:   Override the daemon configuration file path. Equivalent to **--config**.

**OC_RSYNC_SECRETS**
:   Override the daemon secrets file path.

**OC_RSYNC_BRAND**
:   Override the branding identity (for testing and development).

# FILES

**~/.rsync-filter**
:   Per-user filter rules, read automatically when filter support is enabled.

**/etc/oc-rsyncd/oc-rsyncd.conf**
:   Default daemon configuration file. Specifies modules, access control,
    authentication, and other daemon parameters.

**oc-rsyncd.secrets**
:   Daemon password file, referenced by the **secrets file** directive in
    the daemon configuration. Must be readable only by the daemon user
    (mode 0600).

# FILTER RULES

Filter rules control which files are included or excluded from the transfer.
Rules are evaluated in order; the first matching rule wins. Rules can be
specified via **--filter**, **--exclude**, **--include**, **--exclude-from**,
**--include-from**, or per-directory filter files.

A filter rule consists of a prefix character and a pattern:

**- PATTERN**
:   Exclude files matching PATTERN.

**+ PATTERN**
:   Include files matching PATTERN.

**! **
:   Clear all existing filter rules.

**merge FILE**, **. FILE**
:   Read filter rules from FILE.

**dir-merge FILE**, **: FILE**
:   Read filter rules from FILE found in each directory during traversal.

Patterns may contain wildcard characters:

**\***
:   Matches any path component, but stops at slashes.

**\*\***
:   Matches anything, including slashes.

**?**
:   Matches any single character except a slash.

**[**...**]**
:   Matches any character in the set.

# NOTES

## Avoiding double-compression over SSH

SSH stream compression (**ssh -C**, or **Compression yes** in **~/.ssh/config**
or **/etc/ssh/ssh_config**) compresses every byte the SSH session carries. The
rsync wire protocol has its own compression layer, enabled with **-z** /
**--compress** and tuned with **--compress-choice**, **--compress-level**, and
**--compress-threads**. Running both at once feeds already-compressed bytes into
a second compressor: the second pass burns CPU on both peers while shrinking
the stream by almost nothing, and on CPU-bound hosts throughput typically drops
by 20-40%.

Pick one layer. For most workloads prefer rsync's own compression: **oc-rsync**
negotiates zstd when both peers support it (falling back to zlib), which is
usually faster and tighter than SSH's zlib stream, and individual file suffixes
can be skipped via **--skip-compress**. If SSH compression must stay on for
reasons outside your control, drop **-z** from the rsync invocation instead.

**oc-rsync** emits a one-line warning when it detects **-C** or
**-o Compression=yes** in the **--rsh** / **-e** argv it builds for the SSH
child, but it does not parse **~/.ssh/config** or **/etc/ssh/ssh_config**, so a
**Compression yes** directive set there is invisible to the warning. If
throughput looks CPU-bound on an SSH transfer, check those files as well.

## TLS for daemon connections

The daemon protocol is plaintext, matching upstream rsync: the daemon provides
authentication (**auth users**) but not encryption. To encrypt daemon traffic,
place the daemon behind an SSL proxy (**stunnel**, **HAProxy**, or **nginx**)
that terminates TLS, binding the daemon to localhost so only the proxy reaches
it - the same model as upstream **rsync-ssl**.

When built with the **client-tls** feature, the **--ssl** flag connects to such
an SSL-proxied daemon without external client tooling. Default certificate
verification uses the Mozilla root CA bundle; override with **--ssl-ca-cert**.
client-tls uses rustls (pure Rust, no OpenSSL linking) and supports TLS 1.2 and
TLS 1.3.

## SSH stderr socketpair channel

Default builds drain the SSH child's stderr through an anonymous pipe on a
dedicated reader thread. The **ssh-socketpair-stderr** cargo feature swaps the
pipe for a **socketpair(AF_UNIX, SOCK_STREAM)** and, when the async transport
is in use, hands the parent end to an epoll/kqueue-integrated drain. Wake-up
and shutdown become event-driven instead of timeout-bounded, and the larger
socket buffer absorbs bursty remote shells without dropping diagnostic lines.

Enable it with:

    cargo build --features ssh-socketpair-stderr

Linux is the supported target; the Windows shim is pending. See
**docs/design/socketpair-stderr-channel.md** in the source distribution.

When the runtime cannot honour the feature, **oc-rsync** emits one of three
one-shot warnings. Each fires at most once per process; the substrings below
are the operator-grep contract. Sync-path warnings go to stderr; async-path
warnings go to the **tracing** target **ssh::stderr**.

**SSH stderr async drain unavailable on this platform**
:   The kernel rejected **socketpair(AF_UNIX, SOCK_STREAM, 0)** - typically
    **EMFILE**, **ENFILE**, **EPERM**, or **ENOSYS** under seccomp. The session
    falls back to **Stdio::piped()**. Raise the per-process fd limit or relax
    the sandbox to restore the socketpair drain.

**SSH stderr socketpair partially set up**
:   The socketpair allocated but **dup(2)** on the parent half failed, usually
    **EMFILE**. The drain still reads from the socketpair, but **shutdown_read**
    becomes a no-op and the drain thread is bounded by a 50 ms timeout at
    child exit. Investigate fd pressure in the parent process.

**SSH stderr async drain falling back to Stdio::inherit()**
:   The async transport could not stand up the socketpair, so the SSH child's
    stderr is wired straight to the parent terminal. **stderr_capture()**
    returns empty for this session; consume diagnostics from the parent's own
    stderr instead.

## Interrupt behavior with --partial and --delay-updates

When **oc-rsync** is interrupted mid-transfer (SIGINT, SIGTERM, connection
drop, or I/O error), its cleanup behavior depends on which options are
active:

**No --partial (default)**
:   The temporary file for the in-progress transfer is deleted. The
    destination tree is unchanged - either the previous version of the file
    remains or no file exists if this was an initial copy.

**--partial**
:   The incomplete temporary file is renamed to the final destination path.
    On Unix, the file's modification time is stamped to the epoch
    (1970-01-01 00:00:00 UTC). This guarantees that a subsequent
    **--update** run will not skip the file, because the epoch mtime is
    always older than any real source file. On the next run, **oc-rsync**
    uses the partial file as a basis for delta transfer and only fetches the
    missing data.

**--partial-dir**=*DIR*
:   The incomplete temporary file is moved into *DIR* rather than to the
    final destination. The destination tree remains unchanged. On a
    subsequent run, the receiver finds the partial file in *DIR* and uses it
    as a delta basis.

**--delay-updates**
:   All files that completed before the interrupt remain in the staging
    directory (**.~tmp~** by default, or the directory given by
    **--partial-dir**). No file is renamed to its final destination. The
    destination tree is unchanged. A subsequent run picks up the staged
    files and resumes.

### Cross-platform note

Windows NTFS cannot represent a modification time of 1970-01-01 00:00:00
UTC (the epoch). On Windows, when **--partial** retains a file after an
interrupt, the file keeps whatever mtime it had at the time of
interruption. This means **--update** may skip the partial file if its mtime
is newer than the source. To force re-transfer on Windows, either omit
**--update** on the retry run, or use **--partial-dir** instead so that the
partial file does not occupy the final destination path.

# COMPATIBILITY

**oc-rsync** is wire-compatible with upstream rsync and can operate with:

- Upstream rsync 2.6.9 (protocol 29) - daemon push/pull validated via RP28 series
- Upstream rsync 3.0.9 (protocol 30)
- Upstream rsync 3.1.3 (protocol 31)
- Upstream rsync 3.4.1 (protocol 32)
- Upstream rsync 3.4.2 (protocol 32)

Both as a client connecting to upstream rsync servers and as a server
accepting connections from upstream rsync clients.

# SUPPORTED RSYNC PROTOCOL VERSIONS

**oc-rsync** negotiates `protocol_version` per upstream rsync, defaults to 32,
and supports back-negotiation to protocol 28 inclusive. The peer's advertised
protocol determines the version used; the lower of the two is selected.

| Protocol | Upstream version | Status in oc-rsync | Notes |
|----------|------------------|---------------------|-------|
| 32       | 3.4.x            | Full support        | Primary target; all features negotiated |
| 31       | 3.2.x - 3.3.x    | Full support        | Verified via interop matrix |
| 30       | 3.1.x            | Full support        | Verified via interop matrix |
| 29       | 3.0.x            | Full support        | Verified via interop matrix |
| 28       | 2.6.x            | Wire-level support  | Validated via wire-byte regression tests; full interop with rsync 2.6.9 tracked under the RP28 series |
| <= 27    | <= 2.5.x         | Not supported       | Pre-dates protocol cleanup; not tested |

Protocol back-negotiation gates appear in
`crates/protocol/src/wire/compressed_token/zlib_codec.rs` and sibling codec
and capability files. See the README "Supported rsync protocol versions"
section for the same information rendered for browser readers.

# SECURITY

Protocol-handling code enforces **#![deny(unsafe_code)]** in Rust, eliminating
buffer overflow, use-after-free, and uninitialized memory vulnerabilities.
Unsafe code is restricted to SIMD checksum implementations, platform I/O
operations, and OS-level metadata FFI, all with safe fallbacks.

**oc-rsync** is not vulnerable to known upstream rsync CVEs including
CVE-2024-12084 through CVE-2024-12088 and CVE-2024-12747.

For daemon mode, use **chroot**, restrict modules to required paths, enable
authentication, and prefer read-only modules where possible.

For full security details, see the SECURITY.md file in the source distribution.

## Daemon Sandboxing

The daemon layers two independent defenses around each module connection. Both
engage automatically; neither requires `oc-rsyncd.conf` changes.

**Landlock LSM defense-in-depth (Linux)**
:   On Linux 5.13+ with the `landlock` Cargo feature compiled in (default for
    Linux distro builds), the daemon engages a per-connection kernel
    allowlist over `module.path` immediately after
    `apply_module_privilege_restrictions` returns. The allowlist is layered
    above the SEC-1 `*at`-syscall chain
    (**openat2**(2) with `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS`,
    **fstatat**(2), **unlinkat**(2), **mkdirat**(2), **symlinkat**(2),
    **linkat**(2), **fchmodat**(2), **fchownat**(2), **utimensat**(2),
    **renameat**(2)) and prevents any future missed call site from reaching
    paths outside the module tree, because the kernel rejects the syscall
    regardless of which userspace routine issued it. Best-effort ABI
    downgrade per `landlock::ABI::set_best_effort(true)` selects the highest
    ruleset the running kernel understands:

    - **v3** on Linux 6.2+: READ, WRITE, CREATE, DELETE, RENAME, SYMLINK,
      REFER, TRUNCATE.
    - **v2** on Linux 5.19+: v3 minus TRUNCATE.
    - **v1** on Linux 5.13+: v2 minus REFER (no cross-hierarchy rename).
    - On pre-5.13 kernels the daemon logs a single WARN and continues with
      the SEC-1 `*at` chain as the sole defense.

    The active ABI level is recorded in the daemon log so operators can
    confirm v3 enforcement on production kernels.

**Hook inheritance**
:   Name converters and **pre-xfer-exec** / **post-xfer-exec** hooks spawned
    by the daemon inherit the Landlock ruleset, because `restrict_self()`
    applies to the whole thread and to every child process forked from it.
    Hooks therefore **cannot** access paths outside `module.path`, even when
    the unix user they run as has filesystem permission elsewhere. If a hook
    requires auxiliary paths (a shared log directory, a state file under
    **/var/lib**, an audit pipe), either build the daemon without the
    `landlock` Cargo feature or relocate the auxiliary path inside the
    module tree so the kernel allowlist covers it.

**Client path rejection**
:   `--temp-dir`, `--partial-dir`, and `--backup-dir` paths supplied by the
    client are validated against `module.path` at the wire-protocol layer.
    Paths that resolve outside the module tree are rejected with an
    `@ERROR` reply before the transfer begins. This is stricter than
    upstream rsync, which silently widens the chroot to accommodate
    out-of-tree client operands. The stricter posture matches the SEC-1
    mitigation goal: every receiver write stays under the dirfd-anchored
    sandbox. Clients that need auxiliary paths must place them inside the
    module tree.

Refer to **SECURITY.md** in the source distribution for full CVE status,
including the SEC-1 (CVE-2026-29518 / CVE-2026-43619) progress matrix and
the SEC-1.p Landlock design note.

# SSH TRANSPORT

**oc-rsync** uses an embedded **russh** SSH client for SSH transport. The
default code path does not spawn an external **ssh**(1) subprocess; the
SSH state machine (transport, channel, auth context) lives inside the
**oc-rsync** process for the full lifetime of the transfer.

**Authentication**
:   Key-based authentication is supported for RSA, ED25519, and ECDSA key
    types. Password authentication is also supported. Private keys are
    read from the conventional locations under **~/.ssh/id_\***, and
    per-host settings (**HostName**, **User**, **Port**,
    **IdentityFile**, **ProxyJump** and the SSC-3 / SSC-4 subset of
    other directives) are read from **~/.ssh/config** via the embedded
    **ssh2-config** parser.

**Environment**
:   The **SSH_AUTH_SOCK** environment variable is honored, so ssh-agent
    forwarding works for authentication when an agent socket is
    available. See also the **ENVIRONMENT** section above.

**Compression**
:   A **Compression yes** directive in **~/.ssh/config** is honored by
    the embedded client (SSC-4 series). When the user also requests
    rsync wire compression via **-z** / **--compress**, a one-time
    startup warning is emitted so the operator can pick one layer and
    avoid double-compressing the stream (SSC-1 series). See the
    *Performance tuning* section of the README for the operator-facing
    guidance and the exact warning substring.

**Limitations**
:   Some SSH features are not yet fully supported by the embedded
    client. Notable cases include certificate-based authentication, the
    SSH-2 **ControlMaster** multiplexing feature, and exotic key
    exchange methods outside the OpenSSH defaults. The full limitation
    surface and migration plan are tracked under the RUSSH series; see
    in particular **docs/design/russh-async-native-path.md** for the
    planned async-native transport and
    **docs/design/russh-async-native-back-compat-shim.md** for the
    back-compat shim. If you hit an unsupported SSH feature in
    practice, please open an issue against the project repository.

See also the **SSH transport (russh)** section of the README for the
operator-facing summary and a worked example.

# LINUX IO_URING SUPPORT

**oc-rsync** uses io_uring opportunistically on Linux when the running
kernel exposes the required opcode set. The hard floor for any io_uring
dispatch at all is **Linux 5.6** (set by `MIN_KERNEL_VERSION` in
`crates/fast_io/src/io_uring/config.rs`); below that kernel, and on every
non-Linux platform, **oc-rsync** falls back transparently to standard
**read**(2) / **write**(2) and to the platform-specific data paths
(IOCP-backed writes and sockets on Windows, standard buffered I/O
elsewhere). Above the 5.6 floor, individual opcodes are probed at
runtime; opcodes the kernel rejects are individually downgraded to a
documented fallback without aborting the transfer.

The table below lists the io_uring opcodes dispatched by the
**oc-rsync** fast-I/O subsystem, the kernel version that first ships
each opcode, and the path taken when the opcode is unavailable. The
inventory mirrors `docs/audit/iouring-opcode-kernel-floor.md`; refer
to that document for the per-call-site source map.

| Opcode | Min kernel | Fallback when unavailable |
|--------|-----------|----------------------------|
| **IORING_OP_FSYNC** | 5.1 | Standard **fsync**(2) via the non-io_uring **disk_commit** writer (selected when ring construction fails). |
| **IORING_OP_READ_FIXED** / **IORING_OP_WRITE_FIXED** | 5.1 | Plain **IORING_OP_READ** / **IORING_OP_WRITE** when no registered-buffer lease is available; below the 5.6 ring floor, libc **read**(2) / **write**(2). |
| **IORING_OP_POLL_ADD** | 5.1 | Caller receives **io::ErrorKind::Unsupported** and reverts to blocking writes outside io_uring. |
| **IORING_OP_ASYNC_CANCEL** (user-data form) | 5.5 | Stub returns **Unsupported**; cancel becomes a no-op and the request runs to completion. |
| **IORING_OP_ASYNC_CANCEL** (fd-targeted form) | 5.19 | Downgrades to the 5.5 user-data cancel form when fd-targeted cancel is rejected. |
| **IORING_OP_LINK_TIMEOUT** | 5.5 | Chained **PollAdd** still arms; the timeout safety rail is treated as best-effort. |
| **IORING_OP_READ** / **IORING_OP_WRITE** | 5.6 | Below the 5.6 ring floor the whole io_uring path is bypassed and standard **read**(2) / **write**(2) executors take over. |
| **IORING_OP_SEND** / **IORING_OP_RECV** | 5.6 | Socket reader/writer factory returns **Unsupported**; transport reverts to blocking **send**(2) / **recv**(2). |
| **IORING_OP_STATX** | 5.11 | Stub returns **Unsupported**; callers fall back to the libc **statx**(2) / **stat**(2) syscall, and the receiver fast path that batches stat calls effectively becomes serial. |
| **IORING_OP_RENAMEAT** | 5.11 | Runtime probe rejects the SQE; **io_uring_ops::try_io_uring_rename** falls back to libc **renameat2**(2). |
| **IORING_OP_LINKAT** | 5.15 | Runtime probe rejects the SQE; **io_uring_ops::try_io_uring_hardlink** falls back to libc **linkat**(2). |
| **IORING_REGISTER_PBUF_RING** / **IORING_UNREGISTER_PBUF_RING** | 5.19 | Reader and writer paths skip the kernel-side buffer ring; the legacy provide-buffers path runs (or, if that is also missing, plain **IORING_OP_READ** against an owned buffer). |
| **IORING_OP_SEND_ZC** | 6.0 | Falls back to **IORING_OP_SEND**. Default builds additionally gate this dispatch behind the **iouring-send-zc** cargo feature and downgrade silently to **SEND** when the feature is not compiled in, even on 6.0+. |

The full feature tier - all opcodes available, including **SEND_ZC**
and fd-targeted cancel - effectively requires **Linux 6.0** with the
**iouring-send-zc** feature enabled. On RHEL 8 era kernels (4.18) and
on every non-Linux target, **oc-rsync** runs end-to-end without
io_uring by selecting the standard syscall executors throughout.

See the README section on Linux io_uring kernel-tier support for the
operator-facing summary, and
**docs/audit/iouring-opcode-kernel-floor.md** in the source
distribution for the complete per-opcode dispatch-site inventory and
the kernel-tier table that this section is derived from.

# SEE ALSO

rsync(1), rsyncd.conf(5), ssh(1)

Project homepage: <https://github.com/oferchen/rsync>

# AUTHORS

oc-rsync contributors. See the project repository for a full list.

# LICENSE

GNU General Public License v3.0 or later.
