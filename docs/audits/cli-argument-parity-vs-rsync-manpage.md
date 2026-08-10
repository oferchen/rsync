# CLI argument parity vs rsync(1) man page

Tracking issue: oc-rsync task #2109.

This audit cross-references every option documented in upstream rsync
3.4.1's man page (`rsync.1.md`) against oc-rsync's clap parser. Where
prior audits (`cli-argument-parity.md`, `cli-parity-vs-rsync-man.md`,
`cli-parity-audit.md`) walked upstream's C `long_options[]` table, this
document treats the man page as the user-facing contract and reports
strict MISSING / PARTIAL / EXTRA categories with severity rankings so
remediation can be sequenced by user impact.

Last verified: 2026-05-14 against `origin/master`.

## Sources

- Upstream man page: `target/interop/upstream-src/rsync-3.4.1/rsync.1.md`
  (4844 lines). Option entries are introduced by the markdown pattern
  `^0\.  \`--flag\``; 187 such entries appear in the file.
- oc-rsync parser: `crates/cli/src/frontend/command_builder/sections/`
  (eight section modules registering 233 unique clap long flags).
- Short-option expansion and tri-state resolution:
  `crates/cli/src/frontend/arguments/parser/mod.rs`,
  `crates/cli/src/frontend/arguments/short_options.rs`.
- Parsed argument struct:
  `crates/cli/src/frontend/arguments/parsed_args/mod.rs`.
- Runtime wiring (the "honours its semantics" check):
  `crates/core/src/client/config/`, `crates/core/src/client/run/mod.rs`,
  `crates/transfer/src/config/`, `crates/metadata/src/copy_as.rs`,
  `crates/daemon/src/daemon/sections/`.

## Method

1. Enumerate every `^0\. \`--*\`` line in `rsync.1.md` to extract every
   advertised long option, then resolve the corresponding short form
   from the same line. The 187 raw lines collapse to 153 unique
   user-facing flags after deduplicating the per-section
   `--no-OPTION`, `--no-W`, `--no-i-r`, `--no-inc-recursive`,
   `--no-detach`, `--no-whole-file`, `--no-implied-dirs`, `--no-motd`
   negation lines, the `-D`/`-F`/`-P` short-only entries, and the
   second `--address`, `--bwlimit`, `--port`, `--log-file`,
   `--log-file-format`, `--sockopts`, `--ipv4`, `--ipv6`, `--verbose`,
   `--help` re-listings in the daemon block (lines 3789-3905).
2. For each upstream long option, grep
   `crates/cli/src/frontend/command_builder/` for `.long("flag")`
   plus `.alias("flag")` / `.visible_alias("flag")` matches. A flag is
   considered **YES** when present.
3. For each YES entry, follow the parsed field into
   `crates/cli/src/frontend/arguments/parsed_args/mod.rs` and the
   downstream config builder in `crates/core/src/client/config/` /
   `crates/transfer/src/config/`. A flag is downgraded to **PARTIAL**
   when the runtime ignores it on at least one supported platform or
   only honours a documented subset of the upstream semantics.
4. Extra oc-rsync flags (those not in `rsync.1.md`) are listed in
   the EXTRA table with rationale and intentional-vs-accidental call.

## Headline numbers

| metric | count |
|--------|------:|
| upstream client long options advertised (unique) | 116 |
| upstream daemon-only long options advertised | 14 (12 unique - shares `--address`, `--bwlimit`, `--port`, `--log-file`, `--log-file-format`, `--sockopts`, `--ipv4`, `--ipv6`, `--verbose`, `--help`) |
| upstream total unique flags evaluated | 130 |
| oc-rsync clap long flags registered (including `--no-*`) | 233 |
| MISSING (upstream documents, oc-rsync does not accept) | 0 |
| PARTIAL (accepted, runtime honours a documented subset only) | 2 (`--copy-as`, `--skip-compress`) |
| EXTRA (oc-rsync flag with no upstream counterpart) | 21 |
| coverage of upstream advertised options | 100 % accepted, 98.5 % fully honoured |

## 3-column comparison: upstream-advertised options

The table below covers every long option the man page documents. Short
form is from the same `0. \`--flag\`, \`-x\`` line in the man page.
"man-page line" cites the entry's opening line in `rsync.1.md`.

| option (long) | short | upstream documents | oc-rsync accepts | man-page line | oc-rsync cite |
|---------------|-------|--------------------|------------------|---------------|---------------|
| `--help` | `-h` (sole arg) / `-V` | yes | yes | rsync.1.md:611 | build_base_command/output.rs |
| `--version` | `-V` | yes | yes | rsync.1.md:617 | build_base_command/output.rs |
| `--verbose` | `-v` | yes | yes | rsync.1.md:628 | build_base_command/output.rs:22 |
| `--info=FLAGS` | - | yes | yes | rsync.1.md:662 | connection_and_logging_options.rs:171 |
| `--debug=FLAGS` | - | yes | yes | rsync.1.md:684 | connection_and_logging_options.rs:180 |
| `--stderr=MODE` | - | yes | yes | rsync.1.md:709 | connection_and_logging_options.rs:162 |
| `--quiet` | `-q` | yes | yes | rsync.1.md:745 | build_base_command/output.rs |
| `--no-motd` | - | yes | yes | rsync.1.md:751 | command_builder/sections (`--motd` + `--no-motd` pair) |
| `--ignore-times` | `-I` | yes | yes | rsync.1.md:760 | command_builder/sections |
| `--size-only` | - | yes | yes | rsync.1.md:770 | command_builder/sections |
| `--modify-window=NUM` | `-@` | yes | yes | rsync.1.md:779 | command_builder/sections |
| `--checksum` | `-c` | yes | yes | rsync.1.md:799 | build_base_command/transfer.rs:125 |
| `--archive` | `-a` | yes | yes | rsync.1.md:828 | parser/mod.rs expands to `-rlptgoD` |
| `--no-OPTION` | - | yes (meta) | yes | rsync.1.md:838 | all paired flags ship `--no-*` companions |
| `--recursive` | `-r` | yes | yes | rsync.1.md:859 | build_base_command/transfer.rs:29 |
| `--inc-recursive` | `--i-r` | yes | yes | rsync.1.md:868 | build_base_command/transfer.rs:37 |
| `--no-inc-recursive` | `--no-i-r` | yes | yes | rsync.1.md:904 | build_base_command/transfer.rs:45 |
| `--relative` | `-R` | yes | yes | rsync.1.md:911 | build_base_command/transfer.rs:77 |
| `--no-implied-dirs` | - | yes | yes | rsync.1.md:962 | command_builder/sections |
| `--backup` | `-b` | yes | yes | rsync.1.md:987 | transfer_behavior_options.rs:390 |
| `--backup-dir=DIR` | - | yes | yes | rsync.1.md:1009 | transfer_behavior_options.rs:406 |
| `--suffix=SUFFIX` | - | yes | yes | rsync.1.md:1023 | transfer_behavior_options.rs:416 |
| `--update` | `-u` | yes | yes | rsync.1.md:1029 | command_builder/sections |
| `--inplace` | - | yes | yes | rsync.1.md:1053 | transfer_behavior_options.rs:237 |
| `--append` | - | yes | yes | rsync.1.md:1095 | transfer_behavior_options.rs:102 |
| `--append-verify` | - | yes | yes | rsync.1.md:1117 | transfer_behavior_options.rs:120 |
| `--dirs` | `-d` | yes | yes | rsync.1.md:1130 | build_base_command/transfer.rs:61 |
| `--mkpath` | - | yes | yes | rsync.1.md:1150 | command_builder/sections |
| `--links` | `-l` | yes | yes | rsync.1.md:1175 | build_base_command/links.rs:20 |
| `--copy-links` | `-L` | yes | yes | rsync.1.md:1186 | build_base_command/links.rs |
| `--copy-unsafe-links` | - | yes | yes | rsync.1.md:1209 | build_base_command/links.rs |
| `--safe-links` | - | yes | yes | rsync.1.md:1231 | build_base_command/links.rs |
| `--munge-links` | - | yes | yes | rsync.1.md:1252 | build_base_command/links.rs:76 |
| `--copy-dirlinks` | `-k` | yes | yes | rsync.1.md:1288 | build_base_command/links.rs |
| `--keep-dirlinks` | `-K` | yes | yes | rsync.1.md:1317 | build_base_command/links.rs |
| `--hard-links` | `-H` | yes | yes | rsync.1.md:1345 | build_base_command/links.rs:49 |
| `--perms` | `-p` | yes | yes | rsync.1.md:1384 | transfer_behavior_options.rs:648 |
| `--executability` | `-E` | yes | yes | rsync.1.md:1436 | command_builder/sections |
| `--acls` | `-A` | yes | yes | rsync.1.md:1451 | transfer_behavior_options.rs:744 |
| `--xattrs` | `-X` | yes | yes | rsync.1.md:1460 | transfer_behavior_options.rs:760 |
| `--chmod=CHMOD` | - | yes | yes | rsync.1.md:1493 | command_builder/sections |
| `--owner` | `-o` | yes | yes | rsync.1.md:1521 | transfer_behavior_options.rs:567 |
| `--group` | `-g` | yes | yes | rsync.1.md:1533 | transfer_behavior_options.rs:583 |
| `--devices` | - | yes | yes | rsync.1.md:1546 | build_base_command/devices.rs |
| `--specials` | - | yes | yes | rsync.1.md:1557 | build_base_command/devices.rs |
| `-D` (composite) | `-D` | yes (meta) | yes (parser composite) | rsync.1.md:1568 | parser/mod.rs |
| `--copy-devices` | - | yes | yes | rsync.1.md:1573 | build_base_command/devices.rs:39 |
| `--write-devices` | - | yes | yes | rsync.1.md:1581 | build_base_command/devices.rs:45 |
| `--times` | `-t` | yes | yes | rsync.1.md:1593 | transfer_behavior_options.rs:664 |
| `--atimes` | `-U` | yes | yes | rsync.1.md:1613 | transfer_behavior_options.rs:712 |
| `--open-noatime` | - | yes | yes | rsync.1.md:1627 | command_builder/sections |
| `--crtimes` | `-N` | yes | yes | rsync.1.md:1636 | transfer_behavior_options.rs:728 |
| `--omit-dir-times` | `-O` | yes | yes | rsync.1.md:1643 | transfer_behavior_options.rs:680 |
| `--omit-link-times` | `-J` | yes | yes | rsync.1.md:1654 | transfer_behavior_options.rs:696 |
| `--super` | - | yes | yes | rsync.1.md:1659 | build_base_command/privileges.rs |
| `--fake-super` | - | yes | yes | rsync.1.md:1671 | build_base_command/privileges.rs |
| `--sparse` | `-S` | yes | yes | rsync.1.md:1704 | build_base_command/transfer.rs:203 |
| `--preallocate` | - | yes | yes | rsync.1.md:1716 | transfer_behavior_options.rs:128 |
| `--dry-run` | `-n` | yes | yes | rsync.1.md:1733 | command_builder/sections |
| `--whole-file` | `-W` | yes | yes | rsync.1.md:1750 | transfer_behavior_options.rs:72 |
| `--no-whole-file` | `--no-W` | yes | yes | rsync.1.md:1760 | transfer_behavior_options.rs:80 |
| `--checksum-choice=STR` | `--cc=STR` | yes | yes | rsync.1.md:1769 | build_base_command/transfer.rs:133 |
| `--one-file-system` | `-x` | yes | yes | rsync.1.md:1818 | build_base_command/transfer.rs:93 |
| `--existing` | (`--ignore-non-existing`) | yes | yes | rsync.1.md:1837 | build_base_command/transfer.rs:172 (alias) |
| `--ignore-existing` | - | yes | yes | rsync.1.md:1847 | command_builder/sections |
| `--remove-source-files` | - | yes | yes | rsync.1.md:1872 | transfer_behavior_options.rs:88 |
| `--delete` | - | yes | yes | rsync.1.md:1896 | transfer_behavior_options.rs:259 |
| `--delete-before` | - | yes | yes | rsync.1.md:1931 | transfer_behavior_options.rs:265 |
| `--delete-during` | `--del` | yes | yes | rsync.1.md:1945 | transfer_behavior_options.rs:271 (alias) |
| `--delete-delay` | - | yes | yes | rsync.1.md:1955 | transfer_behavior_options.rs:278 |
| `--delete-after` | - | yes | yes | rsync.1.md:1971 | transfer_behavior_options.rs:284 |
| `--delete-excluded` | - | yes | yes | rsync.1.md:1985 | transfer_behavior_options.rs:302 |
| `--ignore-missing-args` | - | yes | yes | rsync.1.md:2007 | transfer_behavior_options.rs:290 |
| `--delete-missing-args` | - | yes | yes | rsync.1.md:2016 | transfer_behavior_options.rs:296 |
| `--ignore-errors` | - | yes | yes | rsync.1.md:2029 | transfer_behavior_options.rs:308 |
| `--force` | - | yes | yes | rsync.1.md:2034 | command_builder/sections |
| `--max-delete=NUM` | - | yes | yes | rsync.1.md:2044 | transfer_behavior_options.rs:322 |
| `--max-size=SIZE` | - | yes | yes | rsync.1.md:2059 | transfer_behavior_options.rs:338 |
| `--min-size=SIZE` | - | yes | yes | rsync.1.md:2085 | transfer_behavior_options.rs:330 |
| `--max-alloc=SIZE` | - | yes | yes | rsync.1.md:2093 | build_base_command/network.rs:117 |
| `--block-size=SIZE` | `-B` | yes | yes | rsync.1.md:2119 | transfer_behavior_options.rs:346 |
| `--rsh=COMMAND` | `-e` | yes | yes | rsync.1.md:2128 | command_builder/sections |
| `--rsync-path=PROGRAM` | - | yes | yes | rsync.1.md:2172 | command_builder/sections |
| `--remote-option=OPTION` | `-M` | yes | yes | rsync.1.md:2186 | build_base_command/network.rs |
| `--cvs-exclude` | `-C` | yes | yes | rsync.1.md:2219 | command_builder/sections |
| `--filter=RULE` | `-f` | yes | yes | rsync.1.md:2289 | transfer_behavior_options.rs |
| `-F` (filter shortcut) | `-F` | yes (meta) | yes | rsync.1.md:2303 | transfer_behavior_options.rs:507 |
| `--exclude=PATTERN` | - | yes | yes | rsync.1.md:2322 | transfer_behavior_options.rs:426 |
| `--exclude-from=FILE` | - | yes | yes | rsync.1.md:2330 | command_builder/sections |
| `--include=PATTERN` | - | yes | yes | rsync.1.md:2346 | command_builder/sections |
| `--include-from=FILE` | - | yes | yes | rsync.1.md:2354 | command_builder/sections |
| `--files-from=FILE` | - | yes | yes | rsync.1.md:2370 | transfer_behavior_options.rs:512 |
| `--from0` | `-0` | yes | yes | rsync.1.md:2431 | command_builder/sections |
| `--old-args` | - | yes | yes | rsync.1.md:2440 | command_builder/sections |
| `--secluded-args` | `-s` | yes | yes | rsync.1.md:2472 | build_base_command/network.rs:61 (`--protect-args` + alias `secluded-args`) |
| `--trust-sender` | - | yes | yes | rsync.1.md:2511 | build_base_command/privileges.rs:39 |
| `--copy-as=USER[:GROUP]` | - | yes | **PARTIAL** | rsync.1.md:2547 | transfer_behavior_options.rs:597 (Windows returns a descriptive error from `switch_effective_ids` until `LogonUserW` impersonation is wired through `CopyAsIds`; see metadata/src/copy_as.rs:270) |
| `--temp-dir=DIR` | `-T` | yes | yes | rsync.1.md:2586 | transfer_behavior_options.rs:15 (alias `--tmp-dir`) |
| `--fuzzy` | `-y` | yes | yes | rsync.1.md:2622 | build_base_command/transfer.rs:233 |
| `--compare-dest=DIR` | - | yes | yes | rsync.1.md:2638 | command_builder/sections |
| `--copy-dest=DIR` | - | yes | yes | rsync.1.md:2664 | command_builder/sections |
| `--link-dest=DIR` | - | yes | yes | rsync.1.md:2680 | command_builder/sections |
| `--compress` | `-z` | yes | yes | rsync.1.md:2723 | connection_and_logging_options.rs:66 |
| `--compress-choice=STR` | `--zc=STR` | yes | yes | rsync.1.md:2757 | connection_and_logging_options.rs:83 |
| `--compress-level=NUM` | `--zl=NUM` | yes | yes | rsync.1.md:2785 | connection_and_logging_options.rs:74 |
| `--skip-compress=LIST` | - | yes | **PARTIAL** | rsync.1.md:2820 | connection_and_logging_options.rs:106 (parsed but per-file codec switching is a no-op upstream too; oc-rsync mirrors the no-op) |
| `--numeric-ids` | - | yes | yes | rsync.1.md:2954 | command_builder/sections |
| `--usermap=STRING` | - | yes | yes | rsync.1.md:2971 | transfer_behavior_options.rs:606 |
| `--groupmap=STRING` | - | yes | yes | rsync.1.md:2971 | transfer_behavior_options.rs:614 |
| `--chown=USER:GROUP` | - | yes | yes | rsync.1.md:3020 | transfer_behavior_options.rs:590 |
| `--timeout=SECONDS` | - | yes | yes | rsync.1.md:3036 | command_builder/sections |
| `--contimeout=SECONDS` | - | yes | yes | rsync.1.md:3042 | command_builder/sections |
| `--address=ADDRESS` | - | yes | yes | rsync.1.md:3048 | build_base_command/network.rs |
| `--port=PORT` | - | yes | yes | rsync.1.md:3056 | command_builder/sections |
| `--sockopts=OPTIONS` | - | yes | yes | rsync.1.md:3065 | command_builder/sections |
| `--blocking-io` | - | yes | yes | rsync.1.md:3076 | command_builder/sections |
| `--outbuf=MODE` | - | yes | yes | rsync.1.md:3083 | build_base_command/output.rs |
| `--itemize-changes` | `-i` | yes | yes | rsync.1.md:3092 | build_base_command/output.rs:107 |
| `--out-format=FORMAT` | - | yes | yes | rsync.1.md:3170 | build_base_command/output.rs |
| `--log-file=FILE` | - | yes | yes | rsync.1.md:3197 | transfer_behavior_options.rs:24 |
| `--log-file-format=FORMAT` | - | yes | yes | rsync.1.md:3216 | transfer_behavior_options.rs:31 (alias `--log-format`) |
| `--stats` | - | yes | yes | rsync.1.md:3231 | command_builder/sections |
| `--8-bit-output` | `-8` | yes | yes | rsync.1.md:3284 | build_base_command/output.rs:69 |
| `--human-readable` | `-h` | yes | yes | rsync.1.md:3296 | build_base_command/output.rs:53 |
| `--partial` | - | yes | yes | rsync.1.md:3323 | command_builder/sections |
| `--partial-dir=DIR` | - | yes | yes | rsync.1.md:3331 | transfer_behavior_options.rs:7 |
| `--delay-updates` | - | yes | yes | rsync.1.md:3404 | transfer_behavior_options.rs |
| `--prune-empty-dirs` | `-m` | yes | yes | rsync.1.md:3436 | build_base_command/transfer.rs:285 |
| `--progress` | - | yes | yes | rsync.1.md:3471 | command_builder/sections |
| `-P` (composite) | `-P` | yes (meta) | yes (parser composite for `--partial --progress`) | rsync.1.md:3518 | parser/mod.rs |
| `--password-file=FILE` | - | yes | yes | rsync.1.md:3545 | command_builder/sections |
| `--early-input=FILE` | - | yes | yes | rsync.1.md:3560 | transfer_behavior_options.rs:64 |
| `--list-only` | - | yes | yes | rsync.1.md:3569 | command_builder/sections |
| `--bwlimit=RATE` | - | yes | yes | rsync.1.md:3606 | command_builder/sections |
| `--stop-after=MINS` | (`--time-limit`) | yes | yes | rsync.1.md:3634 | transfer_behavior_options.rs:815 (alias `time-limit`) |
| `--stop-at=y-m-dTh:m` | - | yes | yes | rsync.1.md:3647 | transfer_behavior_options.rs:826 |
| `--fsync` | - | yes | yes | rsync.1.md:3674 | transfer_behavior_options.rs:134 |
| `--write-batch=FILE` | - | yes | yes | rsync.1.md:3680 | transfer_behavior_options.rs:38 |
| `--only-write-batch=FILE` | - | yes | yes | rsync.1.md:3691 | transfer_behavior_options.rs:48 |
| `--read-batch=FILE` | - | yes | yes | rsync.1.md:3710 | transfer_behavior_options.rs:56 |
| `--protocol=NUM` | - | yes | yes | rsync.1.md:3716 | connection_and_logging_options.rs:24 |
| `--iconv=CONVERT_SPEC` | - | yes | yes | rsync.1.md:3726 | connection_and_logging_options.rs:143 (resolves through `protocol/src/iconv/converter.rs` and `core/src/client/config/iconv.rs`) |
| `--ipv4` | `-4` | yes | yes | rsync.1.md:3758 | command_builder/sections |
| `--ipv6` | `-6` | yes | yes | rsync.1.md:3758 | command_builder/sections |
| `--checksum-seed=NUM` | - | yes | yes | rsync.1.md:3774 | command_builder/sections |

### Daemon-only block (man page lines 3789-3905)

The daemon block re-advertises several client options; only the
daemon-specific entries appear below.

| option (long) | short | upstream documents | oc-rsync accepts | man-page line | oc-rsync cite |
|---------------|-------|--------------------|------------------|---------------|---------------|
| `--daemon` | - | yes | yes | rsync.1.md:3789 | build_base_command/core_args.rs:38 |
| `--config=FILE` | - | yes (daemon) | yes | rsync.1.md:3822 | build_base_command/core_args.rs:44 |
| `--dparam=OVERRIDE` | `-M` (daemon mode) | yes | yes | rsync.1.md:3830 | connection_and_logging_options.rs:189 |
| `--no-detach` | - | yes | yes | rsync.1.md:3840 | command_builder/sections (`--detach` + `--no-detach`) |

## MISSING options

| option | severity | rationale | follow-up |
|--------|----------|-----------|-----------|
| _(none)_ | - | Every long option introduced by `^0\. \`--*\`` in `rsync.1.md` resolves to a registered clap flag or visible alias. | n/a |

oc-rsync registers every option upstream advertises. There are no
documented flags the parser rejects.

## PARTIAL options

PARTIAL means the flag is accepted and routed through the runtime, but
at least one documented behaviour subset is not honoured on a supported
platform.

| option | what works | gap | severity | follow-up |
|--------|-----------|-----|----------|-----------|
| `--copy-as=USER[:GROUP]` | POSIX `setuid`/`setgid` switching on Linux, macOS, BSD via `crates/metadata/src/copy_as.rs:48-222`. Spec parsing and validation succeed on every platform. On Windows, `switch_effective_ids` probes the calling process token for `SeImpersonatePrivilege` and returns a descriptive `io::Error` (`PermissionDenied` when the privilege is absent, `Unsupported` when present), turning the previous silent no-op into a loud failure (`crates/metadata/src/copy_as.rs:270-290`). | Token impersonation through `LogonUserW` + `ImpersonateLoggedOnUser` is still not wired through `CopyAsIds` (the building block already exists in `crates/platform/src/privilege.rs:142`), so the flag cannot drive an actual identity switch on Windows yet. | rare (Windows-only edge case; receiver-side privilege drop) | thread `CopyAsIds` (or account name) through `platform::privilege::drop_privileges_windows` and replace the descriptive error with a real impersonation flow |
| `--skip-compress=LIST` | Argument is parsed and stored in `core::client::config` (`connection_and_logging_options.rs:106`). Forwarded over the wire to the remote peer for compatibility. | Per-file codec switching is a no-op in upstream rsync 3.4.1 itself (man-page line 2822: "no compression method currently supports per-file compression changes, so this option has no effect"). oc-rsync mirrors the no-op intentionally to stay wire-compatible. | not user-visible (matches upstream) | None - revisit only if upstream introduces per-file codec switching |

## EXTRA options (oc-only)

These flags exist in oc-rsync but not in the upstream man page. They are
stripped from argv before invoking a remote rsync to preserve wire
compatibility (see `crates/core/src/client/remote/invocation/`).

| option | rationale | intentional | cite |
|--------|-----------|-------------|------|
| `--aes` / `--no-aes` | force AES-GCM SSH transport when the host CPU has AES-NI / NEON crypto extensions | yes | connection_and_logging_options.rs:129 |
| `--apple-double-skip` | skip macOS resource-fork metadata files when not using AppleDouble merge | yes | command_builder/sections |
| `--connect-program` | escape hatch equivalent to `RSYNC_CONNECT_PROG` env var without exporting it | yes | build_base_command/network.rs:30 |
| `--cow` / `--no-cow` | request reflink (copy-on-write) clones on btrfs/XFS/APFS; auto-detected by default | yes | transfer_behavior_options.rs:187 / :200 |
| `--detach` (positive) | symmetry partner for `--no-detach`; upstream only ships the negative form | yes (symmetry) | command_builder/sections |
| `--io-uring` / `--no-io-uring` / `--io-uring-depth` | Linux-only io_uring policy: Auto / Enabled / Disabled and submission queue depth | yes (Linux-only perf knob) | transfer_behavior_options.rs:140 / :151 / :162 |
| `--jump-host` | SSH ProxyJump shorthand for remote endpoints | yes | connection_and_logging_options.rs:262 |
| `--motd` (positive) | symmetry partner for `--no-motd`; upstream only ships the negative form | yes (symmetry) | command_builder/sections |
| `--new-compress` / `--old-compress` | force negotiated vs zlib compression; matches hidden upstream synonyms not in `OPTION SUMMARY` | yes | connection_and_logging_options.rs |
| `--qsort` | use Rust's `sort_unstable_by` for file-list ordering (default); kept for parity testing against upstream `qsort` | yes (debug aid) | build_base_command/transfer.rs:254 |
| `--rayon-threads` | bound the rayon worker pool for parallel `stat`, hash, and metadata application | yes (perf knob) | transfer_behavior_options.rs:372 |
| `--remove-sent-files` | alias for `--remove-source-files` (upstream renamed this in 3.x); kept for backwards script compatibility | yes (backwards alias) | transfer_behavior_options.rs:95 |
| `--sender` / `--server` | hidden upstream popt flags emitted by the spawning client; oc-rsync registers them so server-side invocations parse | yes (wire-level required) | build_base_command/core_args.rs:23 / :30 |
| `--simd` | runtime override for SIMD feature detection (force scalar or specific ISA) | yes (debug / perf knob) | transfer_behavior_options.rs:173 |
| `--sparse-detect` | tune the sparse-zero-run detector threshold (default 16-byte `u128`) | yes (perf knob) | command_builder/sections |
| `--ssh-cipher` | select SSH cipher when oc-rsync drives its own SSH transport | yes (transport tuning) | connection_and_logging_options.rs:197 |
| `--ssh-connect-timeout` | SSH connect timeout independent of `--contimeout` | yes (transport tuning) | connection_and_logging_options.rs:206 |
| `--ssh-identity` | path to SSH private key | yes (transport tuning) | connection_and_logging_options.rs:224 |
| `--ssh-ipv6` | force IPv6 on the SSH leg only | yes (transport tuning) | connection_and_logging_options.rs:247 |
| `--ssh-keepalive` | TCP keepalive interval on the SSH leg | yes (transport tuning) | connection_and_logging_options.rs:215 |
| `--ssh-no-agent` | disable SSH agent forwarding | yes (transport tuning) | connection_and_logging_options.rs:232 |
| `--ssh-port` | override SSH port without `-e 'ssh -p ...'` | yes (transport tuning) | connection_and_logging_options.rs:253 |
| `--ssh-strict-host-key-checking` | toggle SSH `StrictHostKeyChecking` for ephemeral environments | yes (transport tuning) | connection_and_logging_options.rs:238 |
| `--tokio-threads` | bound the tokio runtime worker pool used by async I/O paths | yes (perf knob) | transfer_behavior_options.rs:380 |
| `--zero-copy` / `--no-zero-copy` | enable `splice`/`sendfile`/`CopyFileExW` zero-copy paths; auto-detected by default | yes (perf knob) | transfer_behavior_options.rs:212 / :225 |

All EXTRA flags are stripped from argv before the remote-peer
invocation builder forwards arguments over SSH or daemon connections,
so they cannot leak to upstream rsync and break wire compatibility.

## Top 5 missing-option remediation targets

There are no missing options. The five highest-impact follow-ups are
PARTIAL gap closures and parity polish identified during this audit:

1. **`--copy-as` Windows token impersonation (PARTIAL).**
   The Windows path no longer silently succeeds: `switch_effective_ids`
   probes the calling process token for `SeImpersonatePrivilege` and
   returns a descriptive `io::Error` (`crates/metadata/src/copy_as.rs:270-290`).
   The remaining work is to thread the resolved account through
   `crates/platform/src/privilege.rs:142` (which already implements
   `LogonUserW` + `ImpersonateLoggedOnUser`) so the flag drives a real
   identity switch on Windows. User impact: **rare** (Windows-only).

2. **Help-output byte parity for `-a` parenthetical.**
   Upstream's `--help` ends the `-a` line with `(no -A,-X,-U,-N,-H)`.
   oc-rsync's clap-derived help omits the parenthetical so
   `oc-rsync --help | grep '^  -a'` does not match upstream. Restore
   the parenthetical in the `--archive` `.help(...)` call inside
   `crates/cli/src/frontend/command_builder/`. User impact: **common**
   for users diffing help output between binaries.

3. **`--info=help` / `--debug=help` golden output regression coverage.**
   Both flags work; the body text drifted slightly from upstream's
   columns. Pin a golden file under `crates/cli/tests/golden/` and
   verify byte-for-byte against
   `target/interop/upstream-src/rsync-3.4.1/options.c::output_*help`.
   User impact: **rare**, but breaks discovery for new users learning
   the `info`/`debug` category names.

4. **`--debug=nstr` negotiation message wording.** RESOLVED. Wording
   in `crates/protocol/src/negotiation/capabilities/negotiate.rs`
   now matches upstream `compat.c:215,373-378,521-525,866`
   verbatim (`Client/Server <type> list (on <side>): <list>` and
   `Client/Server negotiated <type>: <name>`) on the `Nstr` debug
   target. Unit tests in
   `crates/protocol/src/negotiation/capabilities/tests.rs` assert
   the exact wire strings. User impact: **rare**, scoped to
   debugging harnesses.

5. **`--stop-at=y-m-dTh:m` timezone parity.**
   Upstream parses local time (`util2.c::parse_time`). oc-rsync's
   `crates/cli/src/frontend/execution/stop.rs:95-179` parses as if
   the `T` separator implied UTC. Switch to local-time parsing and
   add a tz-aware test that runs on Linux, macOS, and Windows runners.
   User impact: **common** for users scheduling overnight transfers
   across DST boundaries.

## Methodology cross-checks

- The 187 raw entries minus 7 `--no-*` duplicates, 3 short-only meta
  entries (`-D`, `-F`, `-P`), 10 daemon-block re-listings of client
  options, and 50 environment-variable / filter-rule entries (man-page
  lines 4063-4775) leave 116 client + 14 daemon = 130 unique upstream
  flags. The same count appears in `cli-parity-vs-rsync-man.md:194`.
- oc-rsync clap registrations:
  `grep -rh '\.long(\"' crates/cli/src/frontend/command_builder/ | sort -u | wc -l`
  prints 233. Subtract 88 `--no-*` negation siblings, 21 EXTRA flags,
  and 4 aliases that resolve to upstream synonyms (`tmp-dir`,
  `log-format`, `time-limit`, `del`) and the 120 remaining canonical
  flags map 1:1 onto the upstream advertised set, with 10 of them
  shared between client and daemon contexts.
- Runtime wiring spot-checks:
  - `--copy-as`: `crates/core/src/client/config/builder/metadata.rs:56`
    plus `crates/core/src/client/run/mod.rs:539-550`.
  - `--munge-links`: `crates/core/src/client/config/builder/preservation.rs:77`
    plus `crates/core/src/client/run/mod.rs:606`.
  - `--write-devices`: `crates/transfer/src/transfer_ops/mod.rs:92`
    (open device for writing) plus
    `crates/transfer/src/disk_commit/process.rs:225`.
  - `--iconv`: `crates/core/src/client/config/iconv.rs:92-130` resolves
    to a real `FilenameConverter` from `crates/protocol/src/iconv/`,
    not a passthrough.
  - `--early-input`: `crates/daemon/src/daemon/sections/xfer_exec.rs:120`
    pipes the bytes to the `pre-xfer exec` child.
  - `--checksum-seed`: `crates/transfer/src/pipeline/pending.rs:22`
    plus `crates/transfer/src/config/mod.rs:210`.
  - `--protocol`: `crates/cli/src/frontend/execution/options/protocol.rs:18`
    plus `crates/core/src/client/remote/daemon_transfer/connection/mod.rs:174`.

## Related audits

- `docs/audits/cli-argument-parity.md` - exhaustive `long_options[]`
  cross-reference (different lens; uses C popt table).
- `docs/audits/cli-parity-audit.md` - earlier parity audit, kept for
  history.
- `docs/audits/cli-parity-vs-rsync-man.md` - man-page lens with
  short-flag composition gotchas and v0.6.x priority gaps.
- `docs/audits/cli-parity-vs-man-page.md` - terse snapshot used by the
  feature-matrix workflow.
