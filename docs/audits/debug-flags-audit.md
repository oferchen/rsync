# `--debug=FLAGS` Verbosity Matrix vs Upstream rsync 3.4.1

This audit catalogues every `--debug=FLAG` token recognised by upstream
rsync 3.4.1, the maximum level each flag accepts, our parser's handling
of those tokens, and where the flag is consulted at runtime to gate
diagnostic output.

Sources:

- Upstream `target/interop/upstream-src/rsync-3.4.1/options.c` lines
  228-243 (`debug_verbosity[]`), 289-315 (`debug_words[]`),
  427-471 (`parse_output_words()`), 473-510 (`output_item_help()`),
  513-553 (`set_output_verbosity()`/`limit_output_verbosity()`).
- Upstream `target/interop/upstream-src/rsync-3.4.1/rsync.h`
  lines 1414-1462 (`DEBUG_*` index constants, `DEBUG_GTE`).
- All `DEBUG_GTE(...)` call sites grepped under
  `target/interop/upstream-src/rsync-3.4.1/`.
- Our `crates/logging/src/levels/debug.rs`
  (`DebugFlag`, `DebugLevels`).
- Our `crates/logging/src/config.rs`
  (`VerbosityConfig::from_verbose_level`,
  `apply_debug_flag`, `parse_flag_token`).
- Our `crates/cli/src/frontend/execution/flags/debug.rs`
  (`DebugFlagSettings`, `parse_debug_flags`,
  `DEBUG_HELP_TEXT`).
- Our `crates/logging/src/tracing_bridge.rs`
  (`target_to_debug_flag`).

## Master matrix

The **upstream max** column is the largest level `N` ever used in any
`DEBUG_GTE(FLAG, N)` site in the upstream tree (so it bounds the useful
range; upstream caps any user-supplied level at `MAX_OUT_LEVEL = 4`).
The **our max** column is the level cap enforced in
`crates/cli/src/frontend/execution/flags/debug.rs::apply` - tokens
exceeding the cap return `invalid --debug flag '<token>'`.
"Yes" in the **parity** column means the cap matches upstream's
documented range and our gating sites cover the levels actually used.

| Flag       | Upstream max | Our cap | Side bits  | Description (upstream `debug_words[]`)                 | Our gating sites                                                                                                                                                                                                                                            | Parity |
|------------|:------------:|:-------:|------------|--------------------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|:------:|
| ACL        | 1            | u8::MAX | SND+REC    | Debug extra ACL info                                   | None - no `target = "rsync::acl"` emitter; flag accepted but no producer.                                                                                                                                                                                   | Partial |
| BACKUP     | 2 (doc)      | 2       | REC        | Debug backup actions (levels 1-2)                      | None - no emitter on `rsync::backup`; flag accepted, cap matches doc.                                                                                                                                                                                       | Partial |
| BIND       | 1            | u8::MAX | CLI        | Debug socket bind actions                              | None - no emitter on `rsync::bind`.                                                                                                                                                                                                                         | Partial |
| CHDIR      | 1            | u8::MAX | CLI+SRV    | Debug when the current directory changes               | None - no emitter on `rsync::chdir`.                                                                                                                                                                                                                        | Partial |
| CMD        | 2            | 2       | CLI        | Debug commands+options that are issued (levels 1-2)    | None - no emitter on `rsync::cmd`.                                                                                                                                                                                                                          | Partial |
| CONNECT    | 2            | 2       | CLI        | Debug connection events (levels 1-2)                   | None - no emitter on `rsync::connect` is gated through the bridge yet.                                                                                                                                                                                      | Partial |
| DEL        | 3            | 3       | REC        | Debug delete actions (levels 1-3)                      | `crates/engine/src/local_copy/debug_del.rs` (target `rsync::del`, 6 emit sites).                                                                                                                                                                            | Yes |
| DELTASUM   | 4            | 4       | SND+REC    | Debug delta-transfer checksumming (levels 1-4)         | `crates/engine/src/local_copy/debug_deltasum/{checksum.rs, matching.rs, tracer.rs}` (target `rsync::deltasum`, 9 emit sites).                                                                                                                               | Yes |
| DUP        | 1            | u8::MAX | REC        | Debug weeding of duplicate names                       | None - no emitter on `rsync::dup`.                                                                                                                                                                                                                          | Partial |
| EXIT       | 3            | 3       | CLI+SRV    | Debug exit events (levels 1-3)                         | None - no emitter on `rsync::exit`.                                                                                                                                                                                                                         | Partial |
| FILTER     | 3            | 3       | SND+REC    | Debug filter actions (levels 1-3)                      | `crates/filters/src/debug_filter.rs` (target `rsync::filter`).                                                                                                                                                                                              | Yes |
| FLIST      | 4            | 4       | SND+REC    | Debug file-list operations (levels 1-4)                | `crates/engine/src/local_copy/debug_flist.rs` (target `rsync::flist`, 6 emit sites).                                                                                                                                                                        | Yes |
| FUZZY      | 2            | 2       | REC        | Debug fuzzy scoring (levels 1-2)                       | None - no emitter on `rsync::fuzzy`.                                                                                                                                                                                                                        | Partial |
| GENR       | 1            | u8::MAX | REC        | Debug generator functions                              | None - no emitter on `rsync::genr` (existing `rsync::generator` target hits via bridge alias).                                                                                                                                                              | Partial |
| HASH       | 1            | u8::MAX | SND+REC    | Debug hashtable code                                   | None - no emitter on `rsync::hash`.                                                                                                                                                                                                                         | Partial |
| HLINK      | 3 (doc)      | 3       | SND+REC    | Debug hard-link actions (levels 1-3)                   | None - no emitter on `rsync::hlink`.                                                                                                                                                                                                                        | Partial |
| ICONV      | 2            | 2       | CLI+SRV    | Debug iconv character conversions (levels 1-2)         | None - no emitter on `rsync::iconv`.                                                                                                                                                                                                                        | Partial |
| IO         | 4            | 4       | CLI+SRV    | Debug I/O routines (levels 1-4)                        | `crates/protocol/src/debug_io.rs`, `crates/fast_io/src/debug_io.rs`, `crates/rsync_io/src/debug_io.rs` (target `rsync::io`, ~30 emit sites).                                                                                                                | Yes |
| NSTR       | 2            | u8::MAX | CLI+SRV    | Debug negotiation strings                              | None - no emitter on `rsync::nstr`.                                                                                                                                                                                                                         | Partial |
| OWN        | 2            | 2       | REC        | Debug ownership changes in users & groups (levels 1-2) | None - no emitter on `rsync::own`.                                                                                                                                                                                                                          | Partial |
| PROTO      | 1            | u8::MAX | CLI+SRV    | Debug protocol information                             | `crates/protocol/src/debug_trace.rs` plus `target = "rsync::protocol"` events bridged to `DebugFlag::Proto` via `target_to_debug_flag`.                                                                                                                     | Yes |
| RECV       | 1            | u8::MAX | REC        | Debug receiver functions                               | `crates/engine/src/local_copy/debug_recv/{trace_functions.rs, tracer.rs}` (target `rsync::recv`, 9 emit sites).                                                                                                                                             | Yes |
| SEND       | 1            | u8::MAX | SND        | Debug sender functions                                 | `crates/engine/src/local_copy/debug_send.rs` (target `rsync::send`, 9 emit sites).                                                                                                                                                                          | Yes |
| TIME       | 2            | 2       | REC        | Debug setting of modified times (levels 1-2)           | None - no emitter on `rsync::time`.                                                                                                                                                                                                                         | Partial |

The 24-entry table matches `COUNT_DEBUG = DEBUG_TIME + 1` from
`rsync.h`. Side bits use the upstream `W_CLI`, `W_SRV`, `W_SND`, `W_REC`
mask from `options.c:254-257` and represent which roles legitimately
emit the flag - these bits are not enforced on our side; we accept the
flag regardless of role.

## Pseudo flags: `help`, `ALL`, `NONE`

Upstream `parse_output_words()` (options.c:427) applies these tokens
before consulting the `debug_words[]` table:

- `--debug=help` calls `output_item_help()` (options.c:474) which
  prints one line per flag plus the cumulative `-v` mappings, then
  `exit_cleanup(0)`.
- `--debug=ALL` (or `--debug=ALLn`) is recognised when the parser sets
  `len = 0` after the `strncasecmp(str, "all", 3)` branch and then
  loops over every `debug_words[]` entry, assigning each the requested
  level (default 1).
- `--debug=NONE` (or `--debug=NONE0`) sets `len = lev = 0`, which
  zeroes every flag.
- A bare numeric suffix on `ALL` or `NONE` (`ALL2`, `NONE0`) controls
  the level (clamped to `MAX_OUT_LEVEL = 4`).

Our parser
(`crates/cli/src/frontend/execution/flags/debug.rs::DebugFlagSettings::apply`):

| Token              | Upstream behaviour                          | Our behaviour                                                                                  | Parity |
|--------------------|---------------------------------------------|------------------------------------------------------------------------------------------------|:------:|
| `help`             | `output_item_help()`, exit 0                | `settings.help_requested = true`; `DEBUG_HELP_TEXT` is emitted by the caller before exit.       | Yes (text formatted as our help table). |
| `ALL` / `all`      | Set every flag to level 1                   | `enable_all()` sets every flag to `Some(1)`.                                                   | Yes |
| `ALL2`..`ALL4`     | Set every flag to N (clamped to 4)          | Not handled - falls through `apply_flag_and_level` and rejected as unknown token.              | Gap (#2113) |
| `1`                | Treated as `ALL1`                           | `enable_all()` (level 1).                                                                      | Yes |
| `0`                | Treated as `NONE`                           | `disable_all()`.                                                                               | Yes |
| `NONE` / `none`    | Zero every flag                             | `disable_all()` sets every flag to `Some(0)`.                                                  | Yes |
| `no<flag>` / `-<flag>` | `parse_output_words` does not recognise these (level digits only) | `parse_flag_and_level` strips `no`/`-` prefix and forces level 0 (oc-rsync extension).    | Extension - upstream silently rejects, we accept. |
| Unknown token      | `Unknown --debug item: "<tok>"`, exit 1    | `invalid --debug flag '<tok>': use --debug=help for supported flags`, exit 1.                  | Functional parity (different wording). |

## Numbered subcategories (`FLIST3`, `DELTASUM4`, ...)

Upstream `parse_output_words` (options.c:439-445) treats trailing
digits as the level. Token `FLIST3` sets `debug_levels[DEBUG_FLIST] = 3`.
Levels above `MAX_OUT_LEVEL = 4` are silently clamped to 4.

Our `DebugFlagSettings::parse_flag_and_level`
(`crates/cli/src/frontend/execution/flags/debug.rs:291`) trims trailing
ASCII digits, validates that the prefix is in `KNOWN_FLAGS`, parses the
numeric suffix as `u8`, and the per-flag arm in `apply` enforces the
per-flag cap (e.g., `flist` rejects `level > 4`, `cmd` rejects
`level > 2`). For flags whose upstream max is 1, we currently accept
any level value the user supplies (capped only by `u8::MAX` overflow);
upstream silently clamps such inputs to 4. See the per-flag rows above
for the gap.

`VerbosityConfig::apply_debug_flag` (`crates/logging/src/config.rs:233`)
also accepts the `<flag><digit>` form via `parse_flag_token`, with no
per-flag cap. It is invoked from CLI plumbing; the cap therefore
relies on the CLI-layer pre-check in `flags/debug.rs::apply`.

## `-vvvv` interaction

Upstream `set_output_verbosity` (options.c:513) walks the
`debug_verbosity[]` table from `j = 0` up to the current `-v` count and
applies each row at `DEFAULT_PRIORITY = 0`. The rows are
(`options.c:228-235`):

| `-v` count | Added debug levels (upstream `debug_verbosity[]`)                                          |
|:---------:|--------------------------------------------------------------------------------------------|
| 0         | (none - `debug_verbosity[0] = NULL`)                                                       |
| 1         | (none - `debug_verbosity[1] = NULL`)                                                       |
| 2         | `BIND, CMD, CONNECT, DEL, DELTASUM, DUP, FILTER, FLIST, ICONV` (each at level 1)            |
| 3         | `ACL, BACKUP, CONNECT2, DELTASUM2, DEL2, EXIT, FILTER2, FLIST2, FUZZY, GENR, OWN, RECV, SEND, TIME` |
| 4         | `CMD2, DELTASUM3, DEL3, EXIT2, FLIST3, ICONV2, OWN2, PROTO, TIME2`                          |
| 5         | `CHDIR, DELTASUM4, FLIST4, FUZZY2, HASH, HLINK`                                             |

Our `VerbosityConfig::from_verbose_level`
(`crates/logging/src/config.rs:43`) is a hand-rolled cumulative table
that mirrors the upstream rows literally. The match is exhaustive for
levels 0..=5 and treats any level above 5 as 5 (upstream clamps via
`MAX_VERBOSITY = sizeof debug_verbosity / sizeof debug_verbosity[0] - 1
= 5`). `--debug=FLAG` overrides happen after `from_verbose_level`
because `USER_PRIORITY = 2` always beats `DEFAULT_PRIORITY = 0` in
upstream, and our config layers override on top of the verbose
defaults the same way (last-write-wins).

Caveat: upstream's `limit_output_verbosity` (options.c:527) is invoked
during server-side option exchange to cap a peer's user-supplied level
at the implicit `-v` ceiling. We do not yet implement an equivalent
clamp on the receiving end of the wire negotiation - the audit calls
this out so it is not lost.

## Summary

- Token surface (24 flags + `help`/`ALL`/`NONE`/digit suffixes) is
  parsed identically to upstream, modulo the `ALL<N>` gap and our
  `no-<flag>`/`-<flag>` extension.
- Per-flag level caps in `flags/debug.rs::apply` match the upstream
  documentation (`debug_words` help text and the `DEBUG_GTE` call
  sites) for every flag whose upstream max is greater than 1. Flags
  whose upstream max is 1 accept any user input on our side; upstream
  silently clamps to `MAX_OUT_LEVEL = 4`.
- Eight flags currently have at least one production gating site in
  our crates: `DEL`, `DELTASUM`, `FILTER`, `FLIST`, `IO`, `PROTO`,
  `RECV`, `SEND`. The remaining 16 are wired through the parser and
  the bridge but no producer emits messages on their target yet, so
  they parse cleanly but produce nothing - this is acceptable
  (upstream debug output is best-effort) but should be tracked when
  porting the corresponding upstream subsystem.
- `--debug=help` text in `DEBUG_HELP_TEXT`
  (`crates/cli/src/frontend/execution/flags/debug.rs:354`) lists every
  flag with the same per-flag level range upstream prints, including
  the `levels 1-N` annotations.
- `-v`..`-vvvvv` mapping in `VerbosityConfig::from_verbose_level`
  reproduces `debug_verbosity[]` row-for-row up to the documented
  ceiling of 5.
