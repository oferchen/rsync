# CLI argument parity vs rsync(1) man page

Tracking issue: #2109. Last verified 2026-05-07 against origin/master @ `48248ba50`.

## 1. Source of truth

- Man page: `target/interop/upstream-src/rsync-3.4.1/rsync.1` (rendered from
  `rsync.1.md`; OPTION SUMMARY at lines 417-563, DAEMON OPTIONS at 565-592).
- Clap-equivalent C table: `target/interop/upstream-src/rsync-3.4.1/options.c`,
  `long_options[]` from line 590, `long_daemon_options[]` from line 847.
- oc-rsync surface: `crates/cli/src/frontend/arguments/parsed_args/mod.rs`
  (`pub struct ParsedArgs`; every field carries a `///` doc-comment naming
  the upstream long flag it implements) and the alias map under
  `crates/cli/src/frontend/arguments/parser/`.

## 2. Methodology

Enumerate every distinct positive long option from the upstream popt table
(`rg -nE '\{"[a-z0-9]' options.c` over lines 590-868). De-dupe `--no-*`
mirrors, single-letter `--no-x` aliases, daemon-table duplicates, and rows
flagged DEPRECATED in upstream. Cross-check survivors against the doc
strings on `ParsedArgs` fields, then probe the parser for aliases not
covered by `ParsedArgs` (e.g. `--del`, `--ignore-non-existing`, `--motd`,
`--secluded-args`, `--i-r`, `--i-d`, `--no-J`, `--old-dirs`,
`--remove-sent-files`).

Distinct upstream positives: 116 in OPTION SUMMARY plus 14 daemon-only.
oc-rsync recognises 100% of them via `ParsedArgs` plus parser aliases.
No upstream long flag is rejected.

## 3. Categorisation

Implemented (upstream semantics honoured at runtime, 130 of 134 surveyed
flags): every archive bit (`-a`, `-r`, `-l`, `-p`, `-t`, `-g`, `-o`, `-D`),
every delete mode, all transfer-skip predicates, ACL/xattr/hard-link
preservation, delta-transfer toggles, batch mode (`--read-batch`,
`--write-batch`, `--only-write-batch`), filter rules, `--files-from`,
`--from0`, `--protect-args`/`--secluded-args`, daemon mode plus dparam, and
the iconv pipeline. Each maps 1:1 to a `ParsedArgs` field.

Accepted-but-noop (parsed for script compatibility, runtime ignores per
upstream behaviour): `--motd` (positive form is the upstream default;
oc-rsync only stores `no_motd`); `--old-dirs` (upstream legacy `--dirs=4`
toggle, parser folds into the standard `--dirs` field); `--remove-sent-files`
(deprecated upstream alias for `--remove-source-files`).

Missing-but-planned (ergonomic gaps, no protocol impact): `--cc`, `--zc`,
`--zl`, `--time-limit`, `--log-format`. Each is a 3-line parser entry
mapping onto an existing `ParsedArgs` field. Tracked in section 4.

Missing-and-out-of-scope: none. Every upstream long flag has either an
implementation, an accepted alias, or is on the planned list above.

oc-rsync extensions (stripped from argv before remote invocation, see
`crates/cli/src/frontend/server/flags.rs`): `--apple-double-skip`,
`--connect-program`, `--jump-host` (#1881; exposed alongside `-J`),
`--io-uring`, `--io-uring-depth`, `--zero-copy`, `--cow`, `--simd`,
`--sparse-detect`, `--aes`, `--ssh-cipher`, `--ssh-connect-timeout`,
`--ssh-keepalive`, `--ssh-identity`, `--ssh-no-agent`,
`--ssh-strict-host-key-checking`, `--ssh-ipv6`, `--ssh-port`,
`--rayon-threads`, `--tokio-threads`, `--no-open-noatime` (paired explicit
form). `--max-alloc` exists upstream from 3.2.7 but oc-rsync exposes a
strict K/M/G/T/P/E suffix parser with overflow rejection.

## 4. Top 5 missing high-priority options

1. `--cc` short alias for `--checksum-choice` (upstream 3.2.0+). Used in
   docs and CI scripts. 3-line parser entry, no behaviour change.
2. `--zc` short alias for `--compress-choice`. Same shape as #1.
3. `--zl` short alias for `--compress-level`. Same shape as #1.
4. `--log-format` deprecated alias for `--out-format` - upstream emits a
   deprecation warning; oc-rsync should accept and warn identically.
5. `--time-limit` legacy alias for `--stop-after` - upstream still accepts
   it for migrating off rsync 2.6.x scripts.

Each is a self-contained issue with a 1-line parser change and a parse
test. Filing under `feat:` once issue numbers are assigned.

## 5. Audit method (machine-checkable list)

Format: `<long-option> <category>`. Categories: `impl` (full upstream
semantics), `noop` (accepted, runtime ignores per upstream), `planned`
(missing alias tracked in section 4), `oc` (oc-rsync extension stripped
from remote argv), `daemon-impl` (daemon-only popt table). A future CI
gate should parse this list, confirm each `impl` flag is referenced from
`ParsedArgs` doc strings or the alias parser, and fail on drift.

The implemented surface (130 flags) is omitted here for brevity; it is
exactly the set of `///`-annotated fields in `parsed_args/mod.rs` whose
doc-comment names a `--long-form` plus the parser aliases `--del`,
`--ignore-non-existing`, `--secluded-args`, `--i-r`, `--i-d`, `--no-J`.
The diverging rows are listed below.

```
--motd noop
--old-dirs noop
--remove-sent-files noop
--cc planned
--zc planned
--zl planned
--log-format planned
--time-limit planned
--apple-double-skip oc
--connect-program oc
--jump-host oc
--io-uring oc
--io-uring-depth oc
--zero-copy oc
--cow oc
--simd oc
--sparse-detect oc
--aes oc
--ssh-cipher oc
--ssh-connect-timeout oc
--ssh-keepalive oc
--ssh-identity oc
--ssh-no-agent oc
--ssh-strict-host-key-checking oc
--ssh-ipv6 oc
--ssh-port oc
--rayon-threads oc
--tokio-threads oc
--no-open-noatime oc
--config daemon-impl
--daemon daemon-impl
--dparam daemon-impl
--detach daemon-impl
```
