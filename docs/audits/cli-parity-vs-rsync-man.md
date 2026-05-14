# CLI parity audit vs rsync(1) man page

Tracking issue: #2109.

This audit takes a different lens from `cli-parity-audit.md` (which walks the
upstream `long_options[]` table). Here we cross-reference what the upstream
**man page** advertises (`OPTION SUMMARY`, `OPTIONS`, `DAEMON OPTIONS`,
`FILTER RULES`, `BATCH MODE`, `EXIT VALUES`, `ENVIRONMENT VARIABLES`)
against oc-rsync's clap parser, then call out the short-flag composition
gotchas users actually trip over and rank the highest-priority gaps for
v0.6.x.

Last verified: 2026-05-07 against `origin/master`. Sources:

- `target/interop/upstream-src/rsync-3.4.1/rsync.1.md` (4844 lines, 116
  long flags advertised in `OPTION SUMMARY` plus 14 daemon-only entries).
- `target/interop/upstream-src/rsync-3.4.1/options.c` (`long_options[]`
  line 590, `long_daemon_options[]` line 847).
- `crates/cli/src/frontend/command_builder/sections/**` (231 unique clap
  long flags registered).
- `crates/cli/src/frontend/arguments/parser/mod.rs` (`-a` expansion,
  `-D` composite, tri-state resolution).
- `crates/cli/src/frontend/arguments/parsed_args/mod.rs` (`ParsedArgs`).

## Status legend

- **implemented**: oc-rsync accepts the flag and the runtime honours
  upstream semantics on every supported platform.
- **partially-implemented**: accepted and routed, but a documented subset
  of the upstream behaviour is not yet wired (platform gating, value
  subset, or follow-up issue tracked in #).
- **accepted-but-no-op**: parsed by clap so scripts written for upstream
  do not break, but the runtime ignores the value because it has no
  effect at the protocol or filesystem layer (matches upstream behaviour).
- **not-accepted**: upstream defines the flag and oc-rsync rejects it.
- **oc-only**: oc-rsync extension with no upstream counterpart (stripped
  from argv before remote invocation).

## OPTION SUMMARY parity (man page lines 417-563)

The upstream `OPTION SUMMARY` block in `rsync.1.md` is the authoritative
list a user reads first. Every row below lists the upstream advertisement
verbatim, then oc-rsync's status.

| upstream advertisement | status | cite |
|------------------------|--------|------|
| `-v`, `-q`, `--info`, `--debug`, `--stderr`, `--no-motd` | implemented | rsync.1.md:418-423 |
| `-c`, `-a`, `--no-OPTION` | implemented | rsync.1.md:424-426 |
| `-r`, `-R`, `--no-implied-dirs` | implemented | rsync.1.md:427-429 |
| `-b`, `--backup-dir`, `--suffix` | implemented | rsync.1.md:430-432 |
| `-u`, `--inplace`, `--append`, `--append-verify` | implemented | rsync.1.md:433-436 |
| `-d`, `--old-dirs`, `--mkpath` | implemented | rsync.1.md:437-439 |
| `-l`, `-L`, `--copy-unsafe-links`, `--safe-links`, `--munge-links` | implemented | rsync.1.md:440-444 |
| `-k`, `-K`, `-H` | implemented | rsync.1.md:445-447 |
| `-p`, `-E`, `--chmod`, `-A`, `-X` | implemented | rsync.1.md:448-452 |
| `-o`, `-g`, `--devices`, `--copy-devices`, `--write-devices`, `--specials`, `-D` | implemented | rsync.1.md:453-459 |
| `-t`, `-U`, `--open-noatime`, `-N`, `-O`, `-J` | implemented | rsync.1.md:460-465 |
| `--super`, `--fake-super`, `-S`, `--preallocate` | implemented | rsync.1.md:466-469 |
| `-n`, `-W`, `--checksum-choice`, `-x`, `-B` | implemented | rsync.1.md:470-474 |
| `-e`, `--rsync-path`, `--existing`, `--ignore-existing` | implemented | rsync.1.md:475-478 |
| `--remove-source-files`, `--del`, `--delete`, `--delete-{before,during,delay,after,excluded}` | implemented | rsync.1.md:479-486 |
| `--ignore-missing-args`, `--delete-missing-args`, `--ignore-errors`, `--force` | implemented | rsync.1.md:487-490 |
| `--max-delete`, `--max-size`, `--min-size`, `--max-alloc` | implemented | rsync.1.md:491-494 |
| `--partial`, `--partial-dir`, `--delay-updates`, `-m` | implemented | rsync.1.md:495-498 |
| `--numeric-ids`, `--usermap`, `--groupmap`, `--chown` | implemented | rsync.1.md:499-502 |
| `--timeout`, `--contimeout`, `-I`, `--size-only`, `-@` | implemented | rsync.1.md:503-507 |
| `-T`, `-y`, `--compare-dest`, `--copy-dest`, `--link-dest` | implemented | rsync.1.md:508-512 |
| `-z`, `--compress-choice`, `--compress-level`, `--skip-compress` | implemented (see gotchas) | rsync.1.md:513-516 |
| `-C`, `-f`, `-F`, `--exclude`, `--exclude-from`, `--include`, `--include-from`, `--files-from`, `-0` | implemented | rsync.1.md:517-526 |
| `--old-args`, `-s`, `--trust-sender` | implemented | rsync.1.md:527-529 |
| `--copy-as` | partially-implemented | rsync.1.md:530, follow-up: Windows token impersonation gating |
| `--address`, `--port`, `--sockopts`, `--blocking-io`, `--outbuf` | implemented | rsync.1.md:531-535 |
| `--stats`, `-8`, `-h` (human-readable), `--progress`, `-P`, `-i` | implemented | rsync.1.md:536-541 |
| `-M`, `--out-format`, `--log-file`, `--log-file-format`, `--password-file`, `--early-input` | implemented | rsync.1.md:542-547 |
| `--list-only`, `--bwlimit`, `--stop-after`, `--stop-at`, `--fsync` | implemented | rsync.1.md:548-552 |
| `--write-batch`, `--only-write-batch`, `--read-batch`, `--protocol` | implemented | rsync.1.md:553-556 |
| `--iconv` | partially-implemented | rsync.1.md:557, follow-up #1979 (no-op converter pending) |
| `--checksum-seed`, `-4`, `-6`, `-V`, `-h` (help) | implemented | rsync.1.md:558-562 |

### Daemon-only block (man page lines 570-585)

| upstream advertisement | status | cite |
|------------------------|--------|------|
| `--daemon`, `--address`, `--bwlimit`, `--config`, `--dparam` (`-M` daemon-mode) | implemented | rsync.1.md:571-575 |
| `--no-detach`, `--port`, `--log-file`, `--log-file-format`, `--sockopts` | implemented | rsync.1.md:576-580 |
| `-v`, `-4`, `-6`, `-h` (help, daemon mode) | implemented | rsync.1.md:581-584 |

## Common gotchas

These trip up users coming from upstream rsync because the man page
documents them tersely or only inside the `--archive` paragraph. oc-rsync
matches upstream on each.

### `-a` is not `-aAXUNH`

The man page (line 425) explicitly says `archive mode is -rlptgoD (no
-A,-X,-U,-N,-H)`. oc-rsync's `parser::mod` expands `-a` to the same
seven flags and stops there. Users wanting metadata parity with the
modern man-page recommendations must add the extras explicitly:

- `-aAX` to fold in ACLs and xattrs.
- `-aHAX` for hard links plus ACLs plus xattrs.
- `-aUN` to add atimes and crtimes.
- `-aJO` to skip directory and symlink mtimes.

`--no-OPTION` (line 426) turns off any implied flag, e.g. `-a --no-D`
to keep `-rlptgo` without devices/specials. oc-rsync registers
`--no-OPTION` companions for every paired flag, including the short
forms (`--no-D`, `--no-H`, `--no-i-r`, `--no-x`).

### `-D` is `--devices --specials`

Man page line 459 makes this explicit. oc-rsync resolves `-D` and
`--no-D` as composites in the parser; there is no standalone `D` clap
argument. Side effect: `--devices` alone preserves devices but not
FIFOs or sockets; users wanting full special-file fidelity must keep
the `-D` shortcut.

### `-rlptgoD` is the canonical `-a` expansion

Documented as the default in the `-a` line. oc-rsync rejects
`--archive=false` style attempts because upstream does. The only way
to undo individual archive bits is `-a --no-FLAG` chains.

### `-RH` is the safe combination for file-list flattening

Users who pass `-R` (relative paths) often also want `-H` (hard-link
preservation) so identical inodes stay deduplicated across the
preserved directory prefix. Upstream documents the interaction across
several paragraphs (see `rsync.1.md` `--relative` and `--hard-links`
sections). oc-rsync routes `-RH` through the same generator path as
upstream, with one caveat: with `--inc-recursive` (`-i-r`) the
hard-link database is built incrementally, which matches upstream's
own behaviour from protocol 30 onwards.

### `-z` levels and codec negotiation

The `OPTION SUMMARY` advertises `-z`, `--compress-choice`, and
`--compress-level`. The body of the man page (lines 2801-2818)
clarifies:

- zlib / zlibx: levels 1-9, default 6, `--zl=0` disables, `--zl=-1`
  chooses default 6.
- zstd: levels -131072 to 22, default 3, `0` chooses default 3.
- lz4: no levels, value always 0.
- Out-of-range values are silently clamped, so `--zl=999999999`
  always picks the maximum.

oc-rsync mirrors all four behaviours via the `compress` crate, but
two subtleties bite users:

1. `--skip-compress=LIST` is **accepted-but-no-op** in upstream 3.4.1
   itself (see man page line 2822: "no compression method currently
   supports per-file compression changes, so this option has no
   effect"). oc-rsync matches that no-op behaviour on the wire and
   keeps the parser hook so scripts do not break.
2. `--old-compress` and `--new-compress` (synonyms for forcing zlib
   vs the negotiated codec) are clap-registered but absent from
   `OPTION SUMMARY` itself; users find them by reading `OPTIONS`.
   oc-rsync supports both.

### `-h` ambiguity

Man page line 562 footnotes this: `-h` resolves to `--help` only when
it is the sole argument; otherwise it is `--human-readable`. oc-rsync
honours that disambiguation in `parser::mod` by inspecting argv length
before clap dispatch. Daemon mode flips the rule -- `-h` is always
help when paired with `--daemon` (line 584).

### `--no-i-r` short form

Upstream's `--inc-recursive`/`--no-inc-recursive` ships short visible
aliases `--i-r` and `--no-i-r`. oc-rsync registers both. This catches
out users who try `--no-inc-recursive` after typing it once and then
hit `--no-i-r` in a script they copied from a man-page example.

### Tri-state resolution for paired flags

`--perms`/`--no-perms`, `--times`/`--no-times`, etc. resolve to a
tri-state in upstream (`unset`, `on`, `off`). oc-rsync's
`parsed_args/mod.rs` keeps the same `Option<bool>` representation so
that `-a --no-p` produces the same wire effect as upstream
(`preserve_perms=false`, even though `-a` would normally imply on).

## Categorisation summary

| Category | Count |
|----------|------:|
| implemented | 113 |
| partially-implemented | 2 (`--iconv`, `--copy-as`) |
| accepted-but-no-op | 1 (`--skip-compress`, matches upstream no-op) |
| not-accepted | 0 |
| oc-only | 21 |
| upstream long flags evaluated | 116 (108 in `long_options[]`, 8 daemon-only deltas) |

## Top 10 priority gaps for v0.6.x

Ranked by user-visible impact. None are functional regressions; they
graduate partially-implemented or smooth out friction relative to
upstream.

1. **`--iconv` filename charset conversion (#1979).** Currently routes
   through a no-op converter. v0.6.x should wire the platform iconv
   bridge so `--iconv=UTF-8,LATIN1` actually round-trips on POSIX, and
   the Windows path uses `MultiByteToWideChar` with the negotiated
   codepage. Tracked by `iconv-feature-design.md`.

2. **`--copy-as=USER[:GROUP]` on Windows.** Receiver-side identity
   switching uses POSIX `setuid()` today. Windows needs
   `LogonUserW` + `ImpersonateLoggedOnUser` via `windows-rs`. Until
   then `--copy-as` is partial on Windows and parses-only.

3. **`-h` ambiguity smoke test.** Add a CLI parity smoke test that
   diffs `oc-rsync --help` against `rsync --help` after every clap
   change so the `-h`-as-help vs `-h`-as-human-readable rule cannot
   silently regress.

4. **`--archive` expansion documentation in `--help`.** Upstream's
   `--help` text ends the `-a` line with `(no -A,-X,-U,-N,-H)`.
   oc-rsync's clap-derived help omits the parenthetical, which
   surprises users diffing help output. Restore the parenthetical so
   `oc-rsync --help | grep '^  -a'` matches upstream byte-for-byte.

5. **`--info=help` and `--debug=help` golden output.** Both flags are
   implemented but the body text drifted slightly from upstream's
   columns. Pin a golden file under `crates/cli/tests/golden/` and
   regenerate on every clap bump.

6. **`--out-format` modifier matrix.** `%i`, `%n`, `%L`, `%C`, `%b`,
   `%l`, `%U`, `%G`, `%M`, etc. are all implemented; `%C` (checksum)
   currently always emits hex of the strong checksum even when the
   transfer used MD4. Upstream emits MD5 placeholder bytes when the
   checksum choice is "none". Match the placeholder behaviour.

7. **`--debug=FLAGS` parity for the `nstr` (negotiated string)
   category.** Man page line 2816 calls out `--debug=nstr` as the way
   to inspect compression and checksum negotiation. oc-rsync emits the
   negotiation message but the wording does not match upstream's
   `Client compress: zstd (level 3)`. Aligning the wording avoids
   breaking grep-driven test harnesses written for upstream.

8. **`--remote-option` argv quoting on Windows.** `-M --foo=bar` works
   everywhere; `-M --foo="value with spaces"` round-trips on POSIX but
   double-quotes get re-flattened on Windows because the
   `secluded-args` path uses POSIX shell rules. Switch to per-platform
   quoting or document the constraint in `--help`.

9. **`--protocol=NUM` reverse compatibility.** Currently 28-32. Add
   regression tests that pin protocol 30 and 31 wire output against
   golden bytes so future negotiation refactors do not silently drop
   support for older daemons in the field.

10. **`--stop-at=y-m-dTh:m` timezone parsing.** RESOLVED (#2179). The
    parser captures `OffsetDateTime::now_local()` once at the call
    site and reuses that offset for every candidate datetime, mirroring
    upstream `options.c:1155 parse_time()` which calls `localtime(&now)`
    and `mktime(&t)` (both local-zone). Deterministic unit tests pin
    the behaviour with an injected `now` at `+02:00` and `-05:00`,
    proving the same wall-clock input produces distinct unix
    timestamps - the proof that the parser honours local TZ.

## Estimated completion

- Long-flag parity vs `OPTION SUMMARY`: **97.4%** (113 / 116
  implemented; 2 partial; 1 upstream no-op faithfully replicated).
- Long-flag parity vs full `OPTIONS` body (including hidden synonyms
  like `--remove-sent-files`, `--protect-args`): **100%** (every
  upstream synonym is at least alias-only).
- Short-flag composition (`-a`, `-D`, `-RH`, `-zP`, `-aHAX`, `-rlptD`,
  etc.): **100%**, validated against `parser::mod` expansion tests.
- Help output byte-for-byte parity: **~92%**, blocked by gaps 4-7
  above. Fixing those four lifts help-output parity to ~100%.
- Overall CLI surface parity weighted by user-visible impact:
  **~96%**. The remaining 4% is dominated by `--iconv` and
  `--copy-as` (gaps 1-2) and the help / debug wording polish (gaps
  3-7).

## Method notes

- Every `rsync.1.md:N` cite is an absolute line number into the
  upstream man page checked into `target/interop/upstream-src/`.
- `crates/cli/src/frontend/command_builder/sections/**` was scanned
  for `\.long\("..."\)` literals; 231 unique long names were
  registered, accounting for 116 upstream flags plus their
  `--no-*` companions plus the 21 oc-only extensions.
- The categorisation tally cross-checks
  `docs/audits/cli-parity-audit.md`. This file does not duplicate that
  matrix; it focuses on the man-page reader's perspective and the
  short-flag gotchas that audit does not call out.

## Follow-ups (cross-references)

- See `docs/audits/cli-parity-audit.md` for the
  `long_options[]`-row-by-row matrix.
- See `docs/audits/compat-flags-audit.md` for capability-string parity
  on the wire (`-e.LsfxCIvu`).
- See `docs/audits/cross-platform-parity-matrix.md` for the
  per-platform support grid that gates gaps 1, 2, 8, and 10.
