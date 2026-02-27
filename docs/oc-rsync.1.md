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
validated against upstream rsync versions 3.0.9, 3.1.3, and 3.4.1.
Incremental recursion, checksum negotiation (including XXH3 and XXH128),
and all standard wire-format features are supported.

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
:   Preserve POSIX ACLs when supported (Unix only).

**--no-acls**
:   Disable POSIX ACL preservation.

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
:   Keep partially transferred files on error for later resumption.

**--no-partial**
:   Discard partially transferred files on error.

**--partial-dir**=*DIR*
:   Store partially transferred files in *DIR*. Implies **--partial**.

**-T**, **--temp-dir**=*DIR*
:   Store temporary files in *DIR* while transferring. Also available as
    **--tmp-dir**.

**--delay-updates**
:   Put all updated files into place at end of transfer. Uses temporary
    files and renames them after all transfers complete, providing atomic
    updates at the cost of additional disk space.

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
:   Remove excluded destination files during deletion sweeps.

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
:   Compress file data during transfers.

**--no-compress**
:   Disable compression.

**--compress-level**=*NUM*
:   Set compression level (0-9). Level 0 disables compression; level 9
    provides maximum compression. Also available as **--zl**.

**--compress-choice**=*ALGO*
:   Select compression algorithm. Valid values: **zlib**, **zstd**, **lz4**.
    Also available as **--zc**.

**--old-compress**
:   Use old-style (zlib) compression.

**--new-compress**
:   Use new-style compression (typically zstd).

**--skip-compress**=*LIST*
:   Skip compressing files with suffixes in *LIST*.

**--checksum-choice**=*ALGO*
:   Select the strong checksum algorithm. Valid values: **auto**, **md4**,
    **md5**, **xxh64**, **xxh3**, **xxh128**. Also available as **--cc**.

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

**--password-file**=*FILE*
:   Read daemon passwords from *FILE* when contacting rsync:// daemons.

**--no-motd**
:   Suppress daemon message-of-the-day lines.

## Batch Options

**--write-batch**=*PREFIX*
:   Store updated data in batch files named *PREFIX* for later replay.

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

## Performance Options

**--io-uring**
:   Force io_uring for file I/O. Returns an error if io_uring is unavailable.
    Available on Linux 5.6+.

**--no-io-uring**
:   Disable io_uring. Always use standard buffered I/O.

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

# COMPATIBILITY

**oc-rsync** is wire-compatible with upstream rsync and can operate with:

- Upstream rsync 3.0.9 (protocol 30)
- Upstream rsync 3.1.3 (protocol 31)
- Upstream rsync 3.4.1 (protocol 32)

Both as a client connecting to upstream rsync servers and as a server
accepting connections from upstream rsync clients.

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

# SEE ALSO

rsync(1), rsyncd.conf(5), ssh(1)

Project homepage: <https://github.com/oferchen/rsync>

# AUTHORS

oc-rsync contributors. See the project repository for a full list.

# LICENSE

GNU General Public License v3.0 or later.
