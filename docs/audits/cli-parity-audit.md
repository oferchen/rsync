# CLI argument parity audit vs rsync(1)

Tracking issue: #2109.

This audit cross-references every long option in upstream rsync 3.4.1 against
oc-rsync's command-line parser. It is intended as the single source of truth
for "do we accept this flag, and if so, with what semantics?" so reviewers can
land follow-up work without re-deriving the matrix.

Last verified: 2026-05-06 against master @ `60e83fd96`. Sources cross-checked:

- `target/interop/upstream-src/rsync-3.4.1/options.c` (`long_options[]` at
  line 590, `long_daemon_options[]` at line 847).
- `target/interop/upstream-src/rsync-3.4.1/rsync.1.md`.
- `crates/cli/src/frontend/command_builder/sections/build_base_command/`
  (core_args, devices, links, network, output, privileges, transfer).
- `crates/cli/src/frontend/command_builder/sections/transfer_behavior_options.rs`.
- `crates/cli/src/frontend/command_builder/sections/connection_and_logging_options.rs`.
- `crates/cli/src/frontend/arguments/parsed_args/mod.rs` (`ParsedArgs` fields).
- `crates/cli/src/frontend/arguments/parser/mod.rs` (`flags`, tri-state
  resolution, value parsing).

Status legend:

- supported: oc-rsync accepts the flag and routes it through the runtime
  with matching semantics, including the upstream `--no-*` companion when
  one exists.
- partial: oc-rsync accepts the flag but does not yet implement the full
  runtime semantics, only a subset of platforms, or only a subset of values.
- missing: upstream defines the flag and oc-rsync rejects it.
- alias-only: oc-rsync accepts the flag as a re-spelling of another flag
  but does not expose distinct semantics.

oc-rsync extensions (flags that have no upstream counterpart) are listed
separately at the end.

## Method

The columns map upstream's `long_options[]` rows directly. Every short flag
shown matches the second column of the upstream entry. The "notes" column
records how oc-rsync routes the option (which `ParsedArgs` field it sets,
which clap argument id it binds to, and any deviation worth flagging at
review time).

oc-rsync replicates upstream's `--no-LONG`/`--no-S` short-form negation for
all paired options that carry a short flag in `long_options[]`. Where the
upstream alias is identical in spelling and semantics it is recorded under
"notes" rather than getting its own row.

## Transfer / file selection

| Upstream long flag | Short | Status | Notes |
|--------------------|-------|--------|-------|
| `--archive` | `-a` | supported | `archive` clap id; expanded into `-rlptgoD` by `parser::mod`. |
| `--recursive` / `--no-recursive` | `-r` | supported | `recursive`; `--no-r` accepted via `visible_alias("no-r")`. |
| `--inc-recursive` / `--no-inc-recursive` | -- | supported | `inc-recursive`; `--i-r` / `--no-i-r` accepted as visible aliases. |
| `--dirs` / `--no-dirs` | `-d` | supported | `dirs`; `--no-d` alias. |
| `--old-dirs` / `--old-d` | -- | alias-only | Visible alias for `--no-mkpath` (matches upstream `xfer_dirs=4` behaviour). |
| `--perms` / `--no-perms` | `-p` | supported | `perms`; `--no-p` alias. |
| `--executability` | `-E` | supported | `executability`. |
| `--acls` / `--no-acls` | `-A` | supported | POSIX via `exacl`; Windows via `windows-rs`. `--no-A` alias. |
| `--xattrs` / `--no-xattrs` | `-X` | supported | `--no-X` alias. |
| `--times` / `--no-times` | `-t` | supported | `--no-t` alias. |
| `--atimes` / `--no-atimes` | `-U` | supported | `--no-U` alias. |
| `--open-noatime` / `--no-open-noatime` | -- | supported | `open_noatime`. |
| `--crtimes` / `--no-crtimes` | `-N` | supported | `--no-N` alias. macOS/Windows. |
| `--omit-dir-times` / `--no-omit-dir-times` | `-O` | supported | `--no-O` alias. |
| `--omit-link-times` / `--no-omit-link-times` | `-J` | supported | `--no-J` alias. |
| `--modify-window` | `-@` | supported | `modify-window`; integer seconds. |
| `--super` / `--no-super` | -- | supported | `super`. |
| `--fake-super` / `--no-fake-super` | -- | supported | xattr-based privileged-attr storage. |
| `--owner` / `--no-owner` | `-o` | supported | `--no-o` alias. |
| `--group` / `--no-group` | `-g` | supported | `--no-g` alias. |
| `--no-D` | -- | supported | Composite of `--no-devices --no-specials`. |
| `--devices` / `--no-devices` | -- | supported | -- |
| `--copy-devices` | -- | supported | -- |
| `--write-devices` / `--no-write-devices` | -- | supported | -- |
| `--specials` / `--no-specials` | -- | supported | -- |
| `--links` / `--no-links` | `-l` | supported | `--no-l` alias. |
| `--copy-links` | `-L` | supported | -- |
| `--copy-unsafe-links` | -- | supported | -- |
| `--safe-links` | -- | supported | -- |
| `--munge-links` / `--no-munge-links` | -- | supported | -- |
| `--copy-dirlinks` | `-k` | supported | -- |
| `--keep-dirlinks` | `-K` | supported | -- |
| `--hard-links` / `--no-hard-links` | `-H` | supported | `--no-H` alias. |
| `--relative` / `--no-relative` | `-R` | supported | `--no-R` alias. |
| `--implied-dirs` / `--no-implied-dirs` | -- | supported | `--i-d` / `--no-i-d` accepted as visible aliases. |
| `--chmod` | -- | supported | Repeatable; symbolic and octal forms. |
| `--ignore-times` | `-I` | supported | -- |
| `--size-only` | -- | supported | -- |
| `--one-file-system` / `--no-one-file-system` | `-x` | supported | `--no-x` alias; `-xx` count tracked. |
| `--update` | `-u` | supported | -- |
| `--existing` / `--ignore-non-existing` | -- | supported | Visible alias for the same clap id. |
| `--ignore-existing` | -- | supported | -- |
| `--max-size` | -- | supported | K/M/G/T suffixes resolved in `execution::options`. |
| `--min-size` | -- | supported | -- |
| `--max-alloc` | -- | supported | Buffer-pool memory cap. |
| `--sparse` / `--no-sparse` | `-S` | supported | `--no-S` alias. |
| `--preallocate` | -- | supported | `posix_fallocate` / Windows `SetFileValidData`. |
| `--inplace` / `--no-inplace` | -- | supported | -- |
| `--append` / `--no-append` | -- | supported | -- |
| `--append-verify` | -- | supported | -- |
| `--del` | -- | supported | `--delete-during` visible alias. |
| `--delete` | -- | supported | -- |
| `--delete-before` | -- | supported | -- |
| `--delete-during` | -- | supported | -- |
| `--delete-delay` | -- | supported | -- |
| `--delete-after` | -- | supported | -- |
| `--delete-excluded` | -- | supported | -- |
| `--delete-missing-args` | -- | supported | -- |
| `--ignore-missing-args` | -- | supported | -- |
| `--remove-source-files` | -- | supported | -- |
| `--remove-sent-files` | -- | alias-only | Deprecated upstream spelling; mapped to `--remove-source-files`. |
| `--force` / `--no-force` | -- | supported | -- |
| `--ignore-errors` / `--no-ignore-errors` | -- | supported | -- |
| `--max-delete` | -- | supported | -1 = report only. |
| `--whole-file` / `--no-whole-file` | `-W` | supported | `--no-W` alias. |
| `--checksum` / `--no-checksum` | `-c` | supported | `--no-c` alias. |
| `--checksum-choice` / `--cc` | -- | supported | MD4/MD5/XXH3/XXH128. |
| `--checksum-seed` | -- | supported | -- |
| `--block-size` | `-B` | supported | -- |
| `--compare-dest` | -- | supported | Repeatable. Mutually exclusive with `--copy-dest` / `--link-dest`. |
| `--copy-dest` | -- | supported | -- |
| `--link-dest` | -- | supported | -- |
| `--fuzzy` / `--no-fuzzy` | `-y` | supported | `--no-y` alias; `-yy` count tracked. |
| `--compress` / `--no-compress` | `-z` | supported | `--no-z` alias. |
| `--old-compress` | -- | supported | Forces zlib. |
| `--new-compress` | -- | supported | Forces zstd / negotiated codec. |
| `--compress-choice` / `--zc` | -- | supported | -- |
| `--skip-compress` | -- | supported | Comma-separated suffix list. |
| `--compress-level` / `--zl` | -- | supported | 0 disables. |

## Filter

| Upstream long flag | Short | Status | Notes |
|--------------------|-------|--------|-------|
| `--filter` | `-f` | supported | Repeatable. Supports `+`, `-`, `!`, `protect`, `risk`, `merge`, `dir-merge`, `.`, `:` plus modifier flags. |
| `--exclude` | -- | supported | Repeatable. |
| `--include` | -- | supported | Repeatable. |
| `--exclude-from` | -- | supported | Repeatable. |
| `--include-from` | -- | supported | Repeatable. |
| `--cvs-exclude` | `-C` | supported | -- |
| `-F` (filter shortcut) | `-F` | supported | `Count` action; doubles to load receiver-side files. |
| `--files-from` | -- | supported | Repeatable. |
| `--from0` / `--no-from0` | `-0` | supported | -- |

## Output / progress / logging

| Upstream long flag | Short | Status | Notes |
|--------------------|-------|--------|-------|
| `--verbose` / `--no-verbose` | `-v` | supported | `Count` action; `--no-v` alias. |
| `--info` | -- | supported | Repeatable, comma-delimited; `--info=help` honoured. |
| `--debug` | -- | supported | Repeatable, comma-delimited; `--debug=help` honoured. |
| `--stderr` | -- | supported | `errors` / `all` / `client` modes. |
| `--msgs2stderr` / `--no-msgs2stderr` | -- | supported | -- |
| `--quiet` | `-q` | supported | -- |
| `--motd` / `--no-motd` | -- | supported | Daemon listing only. |
| `--stats` | -- | supported | -- |
| `--human-readable` / `--no-human-readable` | `-h` | supported | `Count` action: `-hh`, `-hhh`. `--no-h` alias. |
| `--dry-run` | `-n` | supported | -- |
| `--itemize-changes` / `--no-itemize-changes` | `-i` | supported | `Count` action; `--no-i` alias. |
| `--out-format` / `--log-format` | -- | supported | `--log-format` is the deprecated upstream spelling, kept as a visible alias. |
| `--log-file` | -- | supported | -- |
| `--log-file-format` | -- | supported | -- |
| `--progress` / `--no-progress` | -- | supported | -- |
| `-P` | `-P` | supported | Equivalent to `--partial --progress`. |
| `--partial` / `--no-partial` | -- | supported | -- |
| `--partial-dir` | -- | supported | Implies `--partial`; conflicts with `--inplace`. |
| `--delay-updates` / `--no-delay-updates` | -- | supported | Conflicts with `--inplace` and `--append`. |
| `--prune-empty-dirs` / `--no-prune-empty-dirs` | `-m` | supported | `--no-m` alias. |
| `--8-bit-output` / `--no-8-bit-output` | `-8` | supported | `--no-8` alias. |
| `--list-only` | -- | supported | -- |
| `--outbuf` | -- | supported | `none` / `line` / `block`. |
| `--backup` / `--no-backup` | `-b` | supported | `--no-b` alias. |
| `--backup-dir` | -- | supported | -- |
| `--suffix` | -- | supported | Default `~`. |

## Daemon

| Upstream long flag | Short | Status | Notes |
|--------------------|-------|--------|-------|
| `--daemon` | -- | supported | Triggers oc-rsync daemon mode. |
| `--config` | -- | supported | Path to `oc-rsyncd.conf`. |
| `--dparam` | -- | supported | Repeatable; daemon-only `-M` short form not accepted by client mode (matches upstream). |
| `--detach` / `--no-detach` | -- | supported | -- |
| `--server` | -- | supported | Internal flag set by client when invoking remote. |
| `--sender` | -- | supported | Internal companion to `--server`. |

## Batch

| Upstream long flag | Short | Status | Notes |
|--------------------|-------|--------|-------|
| `--read-batch` | -- | supported | Mutually exclusive with `--write-batch` / `--only-write-batch`. |
| `--write-batch` | -- | supported | Compression unsupported at protocol 28. |
| `--only-write-batch` | -- | supported | -- |

## Network / transport / remote shell

| Upstream long flag | Short | Status | Notes |
|--------------------|-------|--------|-------|
| `--rsh` | `-e` | supported | Capability suffix `e.LsfxCIvu` advertises checksum negotiation. |
| `--rsync-path` | -- | supported | -- |
| `--ipv4` | `-4` | supported | -- |
| `--ipv6` | `-6` | supported | -- |
| `--address` | -- | supported | Local bind address for outgoing connections. |
| `--port` | -- | supported | TCP port for `rsync://` (default 873). |
| `--sockopts` | -- | supported | Comma-separated socket options. |
| `--password-file` | -- | supported | Daemon password reader. |
| `--blocking-io` / `--no-blocking-io` | -- | supported | -- |
| `--contimeout` / `--no-contimeout` | -- | supported | -- |
| `--timeout` / `--no-timeout` | -- | supported | -- |
| `--stop-after` / `--time-limit` | -- | supported | `--time-limit` is the older upstream spelling, kept as alias. |
| `--stop-at` | -- | supported | Conflicts with `--stop-after`. |
| `--bwlimit` / `--no-bwlimit` | -- | supported | K/M/G suffixes. |
| `--protocol` | -- | supported | Force protocol version 28-32. |
| `--secluded-args` / `--no-secluded-args` | `-s` | supported | `--protect-args` / `--no-protect-args` accepted as the upstream-historical aliases; `--no-s` short form supported. |
| `--old-args` / `--no-old-args` | -- | supported | -- |
| `--trust-sender` | -- | supported | -- |
| `--remote-option` | `-M` | supported | Repeatable. |

## Miscellaneous

| Upstream long flag | Short | Status | Notes |
|--------------------|-------|--------|-------|
| `--help` | `-h` (daemon mode) | supported | `--help` always; `-h` resolves to `--human-readable` in client mode, matching upstream. |
| `--version` | `-V` | supported | -- |
| `--mkpath` / `--no-mkpath` | -- | supported | `--no-mkpath` accepts `--old-dirs` / `--old-d` aliases. |
| `--iconv` / `--no-iconv` | -- | partial | Argument is parsed and forwarded but charset conversion currently runs through a no-op converter pending #1979 / iconv-feature-design. |
| `--qsort` | -- | supported | Switches file-list sort to `sort_unstable_by_key`. |
| `--copy-as` | -- | partial | Accepted and forwarded; receiver-side `setuid()` switching is gated behind platform support and not yet wired on Windows. |
| `--usermap` | -- | supported | Repeatable. |
| `--groupmap` | -- | supported | Repeatable. |
| `--chown` | -- | supported | -- |
| `--numeric-ids` / `--no-numeric-ids` | -- | supported | -- |
| `--temp-dir` / `--tmp-dir` | `-T` | supported | -- |
| `--fsync` | -- | supported | -- |
| `--early-input` | -- | supported | -- |

## Daemon-only options (`long_daemon_options[]`)

When invoked with `--daemon`, upstream accepts a reduced option set. oc-rsync
routes daemon mode through the same clap parser, so all of the above flags
remain reachable; the daemon-only forms are listed here for completeness.

| Upstream long flag | Short | Status | Notes |
|--------------------|-------|--------|-------|
| `--address` | -- | supported | Bind address. |
| `--bwlimit` | -- | supported | Per-connection cap. |
| `--config` | -- | supported | -- |
| `--daemon` | -- | supported | -- |
| `--dparam` | `-M` (daemon mode) | supported | `-M` short form active only in daemon mode (matches upstream). |
| `--ipv4` / `--ipv6` | `-4` / `-6` | supported | -- |
| `--detach` / `--no-detach` | -- | supported | -- |
| `--log-file` | -- | supported | -- |
| `--log-file-format` | -- | supported | -- |
| `--port` | -- | supported | -- |
| `--sockopts` | -- | supported | -- |
| `--protocol` | -- | supported | -- |
| `--server` | -- | supported | Internal. |
| `--temp-dir` | `-T` | supported | -- |
| `--verbose` / `--no-verbose` / `--no-v` | `-v` | supported | -- |
| `--help` | `-h` | supported | -- |

## oc-rsync extensions (no upstream counterpart)

These flags exist only in oc-rsync. They are stripped from the argv before
remote invocation so they never appear on the wire.

| Long flag | Short | Notes |
|-----------|-------|-------|
| `--connect-program` | -- | Replacement for the daemon connector when a custom transport is required. |
| `--apple-double-skip` | -- | Excludes macOS AppleDouble (`._foo`) sidecar files. |
| `--io-uring` / `--no-io-uring` | -- | Force or disable io_uring write path on Linux. |
| `--io-uring-depth` | -- | Override io_uring submission queue depth (power of two between `IO_URING_DEPTH_MIN` and `IO_URING_DEPTH_MAX`). |
| `--simd` | -- | Cap SIMD checksum dispatch (`auto`, `avx512`, `avx2`, `sse4`, `neon`, `none`). |
| `--cow` / `--no-cow` | -- | Toggle copy-on-write reflinks for whole-file copies. |
| `--zero-copy` / `--no-zero-copy` | -- | Toggle I/O-level zero-copy syscalls (`sendfile`, `splice`, `copy_file_range`, `SEND_ZC`). |
| `--sparse-detect` | -- | `auto` / `seek` / `map` / `none` hole-detection strategy. |
| `--rayon-threads` | -- | Cap rayon worker pool to N (1-1024). |
| `--tokio-threads` | -- | Cap tokio runtime to N (1-1024); only effective with async features. |
| `--aes` / `--no-aes` | -- | Force AES-GCM cipher selection for SSH. |
| `--ssh-cipher` | -- | Comma-separated cipher preference list for the embedded SSH client. |
| `--ssh-connect-timeout` | -- | Connect timeout for embedded SSH. |
| `--ssh-keepalive` | -- | Keepalive interval for embedded SSH. |
| `--ssh-identity` | -- | Identity file (repeatable). |
| `--ssh-no-agent` | -- | Disable SSH agent authentication. |
| `--ssh-strict-host-key-checking` | -- | `yes` / `no` / `ask`. |
| `--ssh-ipv6` | -- | Prefer IPv6 for the embedded SSH client. |
| `--ssh-port` | -- | Port override for the embedded SSH client. |
| `--jump-host` | -- | Comma-separated proxy-jump hosts (forwarded as `ssh -J`). |

## Tally

- Upstream long flags evaluated: 116 (108 in `long_options[]` plus 8
  daemon-only entries that don't already appear in the client list).
- Supported: 113.
- Partial: 2 (`--iconv` / `--no-iconv`, `--copy-as`).
- Missing: 0.
- Alias-only rows: 3 (`--old-dirs` / `--old-d` for `--no-mkpath`,
  `--remove-sent-files` for `--remove-source-files`, `--protect-args` /
  `--no-protect-args` for `--secluded-args` / `--no-secluded-args`).
- oc-rsync extensions: 21.

## Follow-ups

- #1979 (iconv): finish wiring the filename converter so `--iconv` exits
  partial status.
- `--copy-as` (Windows): graduate from partial to supported once the
  Windows token-impersonation path is on by default.
- Add a non-interactive parity smoke test that diffs `oc-rsync --help`
  against `rsync --help` after every clap change so future drift is caught
  before merge.
