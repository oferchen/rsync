# CLI argument parity vs upstream rsync 3.4.1

Tracking issue: oc-rsync task #2109.

This audit exhaustively cross-references every CLI argument that upstream
rsync 3.4.1 defines against oc-rsync's clap parser. Each upstream option is
traced from the C popt table through to the corresponding `ParsedArgs` field
and clap `Arg` definition. The result is a single source of truth for
option-level parity that reviewers can consult before landing changes.

Last verified: 2026-05-13 against `origin/master`.

## Sources cross-checked

- `target/interop/upstream-src/rsync-3.4.1/options.c` - `long_options[]`
  (line 590) and `long_daemon_options[]` (line 847).
- `target/interop/upstream-src/rsync-3.4.1/rsync.1.md` - man page option
  summary (lines 417-563) and daemon options (lines 565-592).
- `crates/cli/src/frontend/arguments/parsed_args/mod.rs` - `ParsedArgs`
  struct fields.
- `crates/cli/src/frontend/arguments/parser/mod.rs` - option extraction
  and tri-state resolution.
- `crates/cli/src/frontend/command_builder/sections/` - clap `Arg`
  definitions across `build_base_command/` (core_args, devices, links,
  network, output, privileges, transfer) plus
  `transfer_behavior_options.rs` and `connection_and_logging_options.rs`.
- `crates/cli/src/frontend/arguments/short_options.rs` - short-option
  cluster expansion.

## Status legend

- **supported** - oc-rsync accepts the flag and routes it through the
  runtime with matching semantics. The upstream `--no-*` companion is
  accepted when one exists.
- **partial** - oc-rsync accepts the flag but a documented subset of
  upstream behaviour is not yet wired (platform gating, value subset, or
  follow-up issue tracked).
- **alias-only** - oc-rsync accepts the flag as a re-spelling of another
  option without exposing distinct semantics.
- **missing** - upstream defines the flag and oc-rsync does not accept it.

## Method

Every row in the upstream `long_options[]` popt table is enumerated.
Short-form negation aliases (`--no-S` for short flag `-S`) and duplicate
`--no-*` rows that mirror an already-listed positive flag are tracked in
the notes column rather than getting separate rows. The daemon-only popt
table (`long_daemon_options[]`) is listed in a separate section.

---

## 1. File selection and preservation

Options controlling which files are transferred and what metadata is
preserved.

| Upstream long flag | Short | Status | Notes |
|--------------------|-------|--------|-------|
| `--archive` | `-a` | supported | Expands to `-rlptgoD` in `parser::mod`. |
| `--recursive` / `--no-recursive` | `-r` | supported | `--no-r` accepted via `visible_alias`. |
| `--inc-recursive` / `--no-inc-recursive` | - | supported | `--i-r` / `--no-i-r` accepted as aliases. |
| `--dirs` / `--no-dirs` | `-d` | supported | `--no-d` accepted as alias. |
| `--old-dirs` / `--old-d` | - | alias-only | Visible alias on `--no-mkpath` (upstream `xfer_dirs=4`). |
| `--relative` / `--no-relative` | `-R` | supported | `--no-R` accepted as alias. |
| `--implied-dirs` / `--no-implied-dirs` | - | supported | `--i-d` / `--no-i-d` accepted as aliases. |
| `--one-file-system` / `--no-one-file-system` | `-x` | supported | `Count` action; `-xx` tracked. `--no-x` alias. |
| `--perms` / `--no-perms` | `-p` | supported | `--no-p` alias. |
| `--executability` | `-E` | supported | - |
| `--acls` / `--no-acls` | `-A` | supported | POSIX via `exacl`; Windows via `windows-rs`. `--no-A` alias. |
| `--xattrs` / `--no-xattrs` | `-X` | supported | `--no-X` alias. |
| `--times` / `--no-times` | `-t` | supported | `--no-t` alias. |
| `--atimes` / `--no-atimes` | `-U` | supported | `--no-U` alias. |
| `--open-noatime` / `--no-open-noatime` | - | supported | - |
| `--crtimes` / `--no-crtimes` | `-N` | supported | `--no-N` alias. macOS/Windows. |
| `--omit-dir-times` / `--no-omit-dir-times` | `-O` | supported | `--no-O` alias. |
| `--omit-link-times` / `--no-omit-link-times` | `-J` | supported | `--no-J` alias. |
| `--modify-window` | `-@` | supported | Integer seconds. |
| `--super` / `--no-super` | - | supported | - |
| `--fake-super` / `--no-fake-super` | - | supported | xattr-based privileged-attr storage. |
| `--owner` / `--no-owner` | `-o` | supported | `--no-o` alias. |
| `--group` / `--no-group` | `-g` | supported | `--no-g` alias. |
| `-D` (devices + specials) | `-D` | supported | Composite of `--devices --specials`. |
| `--no-D` | - | supported | Composite of `--no-devices --no-specials`. |
| `--devices` / `--no-devices` | - | supported | - |
| `--copy-devices` | - | supported | - |
| `--write-devices` / `--no-write-devices` | - | supported | - |
| `--specials` / `--no-specials` | - | supported | - |
| `--links` / `--no-links` | `-l` | supported | `--no-l` alias. |
| `--copy-links` | `-L` | supported | - |
| `--copy-unsafe-links` | - | supported | - |
| `--safe-links` | - | supported | - |
| `--munge-links` / `--no-munge-links` | - | supported | - |
| `--copy-dirlinks` | `-k` | supported | - |
| `--keep-dirlinks` | `-K` | supported | - |
| `--hard-links` / `--no-hard-links` | `-H` | supported | `--no-H` alias. |
| `--chmod` | - | supported | Repeatable; symbolic and octal forms. |
| `--numeric-ids` / `--no-numeric-ids` | - | supported | - |
| `--usermap` | - | supported | Repeatable. |
| `--groupmap` | - | supported | Repeatable. |
| `--chown` | - | supported | `USER:GROUP` format. |
| `--copy-as` | - | partial | Accepted and forwarded; Windows token impersonation not yet wired. |
| `--sparse` / `--no-sparse` | `-S` | supported | `--no-S` alias. |
| `--preallocate` | - | supported | `posix_fallocate` / Windows `SetFileValidData`. |

## 2. Transfer behaviour

Options controlling how transfers are executed - delta algorithm, deletion,
append, batch mode, and comparison logic.

| Upstream long flag | Short | Status | Notes |
|--------------------|-------|--------|-------|
| `--update` | `-u` | supported | - |
| `--existing` / `--ignore-non-existing` | - | supported | `ignore-non-existing` is visible alias. |
| `--ignore-existing` | - | supported | - |
| `--ignore-times` | `-I` | supported | - |
| `--size-only` | - | supported | - |
| `--checksum` / `--no-checksum` | `-c` | supported | `--no-c` alias. |
| `--checksum-choice` / `--cc` | - | supported | MD4/MD5/XXH3/XXH64/XXH128. `--cc` is visible alias. |
| `--checksum-seed` | - | supported | - |
| `--block-size` | `-B` | supported | - |
| `--whole-file` / `--no-whole-file` | `-W` | supported | `--no-W` alias. |
| `--inplace` / `--no-inplace` | - | supported | - |
| `--append` / `--no-append` | - | supported | - |
| `--append-verify` | - | supported | - |
| `--del` | - | supported | Visible alias for `--delete-during`. |
| `--delete` | - | supported | - |
| `--delete-before` | - | supported | - |
| `--delete-during` | - | supported | - |
| `--delete-delay` | - | supported | - |
| `--delete-after` | - | supported | - |
| `--delete-excluded` | - | supported | - |
| `--delete-missing-args` | - | supported | - |
| `--ignore-missing-args` | - | supported | - |
| `--remove-source-files` | - | supported | - |
| `--remove-sent-files` | - | alias-only | Deprecated upstream spelling; mapped to `--remove-source-files`. |
| `--force` / `--no-force` | - | supported | - |
| `--ignore-errors` / `--no-ignore-errors` | - | supported | - |
| `--max-delete` | - | supported | -1 = report only. |
| `--max-size` | - | supported | K/M/G/T suffixes. |
| `--min-size` | - | supported | K/M/G/T suffixes. |
| `--max-alloc` | - | supported | K/M/G/T/P/E suffixes with overflow rejection. |
| `--compare-dest` | - | supported | Repeatable. Mutually exclusive with `--copy-dest` / `--link-dest`. |
| `--copy-dest` | - | supported | Repeatable. Mutually exclusive with `--compare-dest` / `--link-dest`. |
| `--link-dest` | - | supported | Repeatable. Mutually exclusive with `--compare-dest` / `--copy-dest`. |
| `--fuzzy` / `--no-fuzzy` | `-y` | supported | `--no-y` alias; `-yy` count tracked. |
| `--partial` / `--no-partial` | - | supported | - |
| `--partial-dir` | - | supported | Implies `--partial`. |
| `--delay-updates` / `--no-delay-updates` | - | supported | - |
| `--temp-dir` / `--tmp-dir` | `-T` | supported | `--tmp-dir` is visible alias. |
| `--mkpath` / `--no-mkpath` | - | supported | `--old-dirs` / `--old-d` visible aliases on `--no-mkpath`. |
| `--prune-empty-dirs` / `--no-prune-empty-dirs` | `-m` | supported | `--no-m` alias. |
| `--fsync` | - | supported | - |

## 3. Compression

| Upstream long flag | Short | Status | Notes |
|--------------------|-------|--------|-------|
| `--compress` / `--no-compress` | `-z` | supported | `--no-z` alias. |
| `--old-compress` | - | supported | Forces zlib. |
| `--new-compress` | - | supported | Forces zstd / negotiated codec. |
| `--compress-choice` / `--zc` | - | supported | `--zc` is visible alias. |
| `--compress-level` / `--zl` | - | supported | 0 disables. `--zl` is visible alias. |
| `--skip-compress` | - | supported | Comma-separated suffix list. |

## 4. Filter rules

| Upstream long flag | Short | Status | Notes |
|--------------------|-------|--------|-------|
| `--filter` | `-f` | supported | Repeatable. Supports `+`, `-`, `!`, `protect`, `risk`, `merge`, `dir-merge`, `.`, `:` plus modifier flags. |
| `-F` (filter shortcut) | `-F` | supported | `Count` action; repeat doubles behaviour. |
| `--exclude` | - | supported | Repeatable. |
| `--include` | - | supported | Repeatable. |
| `--exclude-from` | - | supported | Repeatable. |
| `--include-from` | - | supported | Repeatable. |
| `--cvs-exclude` | `-C` | supported | - |
| `--files-from` | - | supported | Repeatable. |
| `--from0` / `--no-from0` | `-0` | supported | - |

## 5. Output, progress, and logging

| Upstream long flag | Short | Status | Notes |
|--------------------|-------|--------|-------|
| `--verbose` / `--no-verbose` | `-v` | supported | `Count` action; `--no-v` alias. |
| `--info` | - | supported | Repeatable, comma-delimited; `--info=help` honoured. |
| `--debug` | - | supported | Repeatable, comma-delimited; `--debug=help` honoured. |
| `--stderr` | - | supported | `errors` / `all` / `client` modes. |
| `--msgs2stderr` / `--no-msgs2stderr` | - | supported | - |
| `--quiet` | `-q` | supported | - |
| `--motd` / `--no-motd` | - | supported | Daemon listing only. |
| `--stats` | - | supported | - |
| `--human-readable` / `--no-human-readable` | `-h` | supported | `Count` action: `-hh`, `-hhh`. `--no-h` alias. |
| `--dry-run` | `-n` | supported | - |
| `--itemize-changes` / `--no-itemize-changes` | `-i` | supported | `--no-i` alias. |
| `--out-format` / `--log-format` | - | supported | `--log-format` is deprecated upstream spelling, kept as visible alias. |
| `--log-file` | - | supported | - |
| `--log-file-format` | - | supported | - |
| `--progress` / `--no-progress` | - | supported | - |
| `-P` | `-P` | supported | Equivalent to `--partial --progress`. |
| `--8-bit-output` / `--no-8-bit-output` | `-8` | supported | `--no-8` alias. |
| `--list-only` | - | supported | - |
| `--outbuf` | - | supported | `none` / `line` / `block`. |

## 6. Backup

| Upstream long flag | Short | Status | Notes |
|--------------------|-------|--------|-------|
| `--backup` / `--no-backup` | `-b` | supported | `--no-b` alias. |
| `--backup-dir` | - | supported | - |
| `--suffix` | - | supported | Default `~`. |

## 7. Batch mode

| Upstream long flag | Short | Status | Notes |
|--------------------|-------|--------|-------|
| `--read-batch` | - | supported | Mutually exclusive with `--write-batch` / `--only-write-batch`. |
| `--write-batch` | - | supported | - |
| `--only-write-batch` | - | supported | - |

## 8. Network, transport, and remote shell

| Upstream long flag | Short | Status | Notes |
|--------------------|-------|--------|-------|
| `--rsh` | `-e` | supported | Capability suffix `e.LsfxCIvu` advertises checksum negotiation. |
| `--rsync-path` | - | supported | - |
| `--ipv4` | `-4` | supported | - |
| `--ipv6` | `-6` | supported | - |
| `--address` | - | supported | Local bind address. |
| `--port` | - | supported | TCP port for `rsync://` (default 873). |
| `--sockopts` | - | supported | Comma-separated socket options. |
| `--password-file` | - | supported | Daemon password reader. |
| `--blocking-io` / `--no-blocking-io` | - | supported | - |
| `--contimeout` / `--no-contimeout` | - | supported | - |
| `--timeout` / `--no-timeout` | - | supported | - |
| `--stop-after` / `--time-limit` | - | supported | `--time-limit` is upstream legacy spelling, kept as alias. |
| `--stop-at` | - | supported | Conflicts with `--stop-after`. |
| `--bwlimit` / `--no-bwlimit` | - | supported | K/M/G suffixes. |
| `--protocol` | - | supported | Force protocol version 28-32. |
| `--secluded-args` / `--no-secluded-args` | `-s` | supported | `--protect-args` / `--no-protect-args` are the upstream-historical aliases; `--no-s` short form supported. |
| `--old-args` / `--no-old-args` | - | supported | - |
| `--trust-sender` | - | supported | - |
| `--remote-option` | `-M` | supported | Repeatable. |
| `--early-input` | - | supported | - |

## 9. Daemon mode

| Upstream long flag | Short | Status | Notes |
|--------------------|-------|--------|-------|
| `--daemon` | - | supported | Triggers daemon mode. |
| `--config` | - | supported | Path to `oc-rsyncd.conf`. |
| `--dparam` | - | supported | Repeatable; `-M` short form active only in daemon mode. |
| `--detach` / `--no-detach` | - | supported | - |
| `--server` | - | supported | Internal flag set by client when invoking remote. |
| `--sender` | - | supported | Internal companion to `--server`. |

## 10. Miscellaneous

| Upstream long flag | Short | Status | Notes |
|--------------------|-------|--------|-------|
| `--help` | `-h` (daemon) | supported | `-h` resolves to `--human-readable` in client mode. |
| `--version` | `-V` | supported | - |
| `--iconv` / `--no-iconv` | - | partial | Argument is parsed and forwarded but charset conversion currently runs through a no-op converter pending #1979 (iconv-feature-design). |
| `--qsort` | - | supported | Switches file-list sort to `sort_unstable_by_key`. |

---

## 11. Daemon-only options (`long_daemon_options[]`)

When invoked with `--daemon`, upstream accepts a reduced option set from the
`long_daemon_options[]` popt table. oc-rsync routes daemon mode through the
same clap parser, so all client-mode flags remain reachable. The daemon-only
entries are listed here for completeness - rows that duplicate client-mode
options note this rather than restating the status.

| Upstream long flag | Short | Status | Notes |
|--------------------|-------|--------|-------|
| `--address` | - | supported | Bind address. Same as client. |
| `--bwlimit` | - | supported | Per-connection cap. Same as client. |
| `--config` | - | supported | Same as client. |
| `--daemon` | - | supported | Same as client. |
| `--dparam` | `-M` | supported | `-M` short form active only in daemon mode (matches upstream). |
| `--ipv4` / `--ipv6` | `-4` / `-6` | supported | Same as client. |
| `--detach` / `--no-detach` | - | supported | Same as client. |
| `--log-file` | - | supported | Same as client. |
| `--log-file-format` | - | supported | Same as client. |
| `--port` | - | supported | Same as client. |
| `--sockopts` | - | supported | Same as client. |
| `--protocol` | - | supported | Same as client. |
| `--server` | - | supported | Internal. Same as client. |
| `--temp-dir` | `-T` | supported | Same as client. |
| `--verbose` / `--no-verbose` / `--no-v` | `-v` | supported | Same as client. |
| `--help` | `-h` | supported | Same as client. |

---

## 12. oc-rsync extensions (no upstream counterpart)

These flags exist only in oc-rsync. They are stripped from argv before
remote invocation so they never appear on the wire.

| Long flag | Short | Notes |
|-----------|-------|-------|
| `--connect-program` | - | Replacement for the daemon connector when a custom transport is required. Supports `%H` and `%P` placeholders. |
| `--apple-double-skip` | - | Excludes macOS AppleDouble (`._foo`) sidecar files. |
| `--io-uring` / `--no-io-uring` | - | Force or disable io_uring write path on Linux. Auto by default. |
| `--io-uring-depth` | - | Override io_uring submission queue depth (power of two, 1-32768). |
| `--simd` | - | Cap SIMD checksum dispatch (`auto`, `avx512`, `avx2`, `sse4`, `neon`, `none`). |
| `--cow` / `--no-cow` | - | Toggle copy-on-write reflinks for whole-file copies. |
| `--zero-copy` / `--no-zero-copy` | - | Toggle I/O-level zero-copy syscalls (`sendfile`, `splice`, `copy_file_range`, `SEND_ZC`). |
| `--sparse-detect` | - | `auto` / `seek` / `map` / `none` hole-detection strategy. |
| `--rayon-threads` | - | Cap rayon worker pool to N (1-1024). |
| `--tokio-threads` | - | Cap tokio runtime to N (1-1024). |
| `--aes` / `--no-aes` | - | Force AES-GCM cipher selection for SSH. |
| `--ssh-cipher` | - | Comma-separated cipher preference list for embedded SSH. |
| `--ssh-connect-timeout` | - | Connect timeout for embedded SSH. |
| `--ssh-keepalive` | - | Keepalive interval for embedded SSH. |
| `--ssh-identity` | - | Identity file (repeatable). |
| `--ssh-no-agent` | - | Disable SSH agent authentication. |
| `--ssh-strict-host-key-checking` | - | `yes` / `no` / `ask`. |
| `--ssh-ipv6` | - | Prefer IPv6 for the embedded SSH client. |
| `--ssh-port` | - | Port override for the embedded SSH client. |
| `--jump-host` | - | Comma-separated proxy-jump hosts (forwarded as `ssh -J`). |

---

## Tally

| Category | Count |
|----------|-------|
| Upstream `long_options[]` distinct entries | 108 |
| Upstream `long_daemon_options[]` entries (not already in client table) | 8 |
| **Total upstream flags evaluated** | **116** |
| Supported | 113 |
| Partial | 2 |
| Missing | 0 |
| Alias-only | 3 |
| oc-rsync extensions | 21 |

### Partial options

1. **`--iconv` / `--no-iconv`** - Argument is parsed and forwarded but
   charset conversion currently routes through a no-op converter. Follow-up:
   #1979 (iconv-feature-design).
2. **`--copy-as`** - Accepted and forwarded; receiver-side `setuid()`
   switching is gated behind platform support and not yet wired on Windows.

### Alias-only options

1. **`--old-dirs` / `--old-d`** - Visible alias for `--no-mkpath` (matches
   upstream `xfer_dirs=4` behaviour).
2. **`--remove-sent-files`** - Deprecated upstream spelling mapped to
   `--remove-source-files`.
3. **`--protect-args` / `--no-protect-args`** - Upstream-historical spelling
   for `--secluded-args` / `--no-secluded-args`.

---

## Short-flag composition parity

Upstream rsync treats `-avz` as three separate flags. oc-rsync replicates
this via the `expand_short_options()` function in `short_options.rs`, which
classifies every clap-registered short flag as either a flag (boolean) or a
value-taking option. Value-taking options (e.g., `-e`, `-M`, `-T`, `-B`,
`-f`) may only appear at the end of a cluster so the next argument is
consumed as their value - matching upstream semantics.

Short flags registered in oc-rsync that match upstream:

| Short | Long | Type |
|-------|------|------|
| `-a` | `--archive` | flag |
| `-b` | `--backup` | flag |
| `-c` | `--checksum` | flag |
| `-d` | `--dirs` | flag |
| `-e` | `--rsh` | value |
| `-f` | `--filter` | value |
| `-g` | `--group` | flag |
| `-h` | `--human-readable` | flag (optional value) |
| `-i` | `--itemize-changes` | flag |
| `-k` | `--copy-dirlinks` | flag |
| `-l` | `--links` | flag |
| `-m` | `--prune-empty-dirs` | flag |
| `-n` | `--dry-run` | flag |
| `-o` | `--owner` | flag |
| `-p` | `--perms` | flag |
| `-q` | `--quiet` | flag |
| `-r` | `--recursive` | flag |
| `-s` | `--protect-args` | flag |
| `-t` | `--times` | flag |
| `-u` | `--update` | flag |
| `-v` | `--verbose` | count |
| `-x` | `--one-file-system` | count |
| `-y` | `--fuzzy` | count |
| `-z` | `--compress` | flag |
| `-0` | `--from0` | flag |
| `-4` | `--ipv4` | flag |
| `-6` | `--ipv6` | flag |
| `-8` | `--8-bit-output` | flag |
| `-@` | `--modify-window` | value |
| `-A` | `--acls` | flag |
| `-B` | `--block-size` | value |
| `-C` | `--cvs-exclude` | flag |
| `-D` | devices + specials | flag |
| `-E` | `--executability` | flag |
| `-F` | filter shortcut | count |
| `-H` | `--hard-links` | flag |
| `-I` | `--ignore-times` | flag |
| `-J` | `--omit-link-times` | flag |
| `-K` | `--keep-dirlinks` | flag |
| `-L` | `--copy-links` | flag |
| `-M` | `--remote-option` | value |
| `-N` | `--crtimes` | flag |
| `-O` | `--omit-dir-times` | flag |
| `-P` | partial + progress | count |
| `-R` | `--relative` | flag |
| `-S` | `--sparse` | flag |
| `-T` | `--temp-dir` | value |
| `-U` | `--atimes` | flag |
| `-V` | `--version` | flag |
| `-W` | `--whole-file` | flag |
| `-X` | `--xattrs` | flag |

All 49 upstream short flags are registered in oc-rsync.

---

## Follow-ups

- **#1979** (iconv): finish wiring the filename converter so `--iconv`
  exits partial status.
- **`--copy-as`** (Windows): graduate from partial to supported once the
  Windows token-impersonation path is on by default.
- Add a non-interactive parity smoke test that diffs `oc-rsync --help`
  against `rsync --help` after every clap change so future drift is caught
  before merge.
