# `--debug=FLAGS` Verbosity Matrix (task #2113)

This audit catalogues every `--debug=FLAG` token recognised by upstream
rsync 3.4.1, the maximum level each flag accepts, oc-rsync's parser
handling of those tokens, and the production gating sites that emit
diagnostic output.

## Sources

- Upstream `target/interop/upstream-src/rsync-3.4.1/options.c` lines
  228-235 (`debug_verbosity[]`), 245 (`MAX_OUT_LEVEL = 4`),
  287-315 (`debug_words[]`), 427-471 (`parse_output_words`),
  473-510 (`output_item_help`), 513-553
  (`set_output_verbosity`/`limit_output_verbosity`).
- Upstream `target/interop/upstream-src/rsync-3.4.1/rsync.h`
  `DEBUG_*` index constants and `DEBUG_GTE` macro.
- Upstream man page `rsyncd.5`/`rsync.1` `--debug=FLAGS` section.
- oc-rsync `crates/logging/src/levels/debug.rs` (`DebugFlag` enum,
  `DebugLevels` per-flag storage with `get`/`set`/`set_all`).
- oc-rsync `crates/logging/src/config.rs` (`VerbosityConfig`,
  `from_verbose_level`, `apply_debug_flag`, `parse_flag_token`).
- oc-rsync `crates/logging/src/tracing_bridge.rs`
  (`target_to_debug_flag`, `level_to_verbosity_level`).
- oc-rsync `crates/cli/src/frontend/execution/flags/debug.rs`
  (`DebugFlagSettings`, `parse_debug_flags`, `DEBUG_HELP_TEXT`,
  `parse_flag_and_level`).

## Upstream level model

Upstream defines five priority bands and hard caps:

- `MAX_OUT_LEVEL = 4` (options.c:245). Any per-flag suffix is clamped
  to 4; level 0 disables.
- `MAX_VERBOSITY = 5` (options.c:237) bounds the cumulative `-v` ladder.
- `DEFAULT_PRIORITY = 0`, `HELP_PRIORITY = 1`, `USER_PRIORITY = 2`,
  `LIMIT_PRIORITY = 3`. User-supplied `--debug=FOO` always wins over
  `-v` defaults; `LIMIT_PRIORITY` is reserved for the server-side
  clamp in `limit_output_verbosity`.

The man page documents `--debug=FLAGS` levels `0` (disabled), `1`, `2`,
`3`, `4`. `0` is equivalent to `NONE` for that flag; `1` is the first
level that emits anything. Levels above the per-flag documented range
are silently clamped by upstream and rejected with an explicit error
by oc-rsync where the per-flag cap is documented.

`ALL` and `NONE` are pseudo-flags handled by upstream
`parse_output_words` before the `debug_words[]` lookup; `help` is
handled even earlier and exits with `output_item_help`.

## Master matrix

The **upstream max** column is the largest level `N` ever passed to
`DEBUG_GTE(FLAG, N)` in the upstream tree. Where the man page or
`debug_words[]` help text annotates a range (`levels 1-N`) the cap is
explicit; otherwise `1` is the only useful level upstream emits.

Side bits use `W_CLI`, `W_SRV`, `W_SND`, `W_REC` masks from
`options.c:254-257` and indicate which roles legitimately emit the
flag - oc-rsync accepts the flag regardless of role.

Status legend:

- **Wired**: parser accepts the flag and at least one production
  gating site emits diagnostic output on the matching tracing target.
- **Parsed-only**: parser accepts the flag, level is stored in
  `DebugLevels`/`DebugFlagSettings`, but no producer emits on the
  matching target. Subsystem port required to lift to Wired.

| Flag     | Upstream max | oc-rsync cap | Side bits | Help text (`debug_words[]`)                            | Verbosity produced                                                      | Status        |
|----------|:------------:|:------------:|-----------|--------------------------------------------------------|-------------------------------------------------------------------------|---------------|
| ACL      | 1            | u8::MAX      | SND+REC   | Debug extra ACL info                                   | None                                                                    | Parsed-only   |
| BACKUP   | 2            | 2            | REC       | Debug backup actions (levels 1-2)                      | None                                                                    | Parsed-only   |
| BIND     | 1            | u8::MAX      | CLI       | Debug socket bind actions                              | None                                                                    | Parsed-only   |
| CHDIR    | 1            | u8::MAX      | CLI+SRV   | Debug when the current directory changes               | None                                                                    | Parsed-only   |
| CONNECT  | 2            | 2            | CLI       | Debug connection events (levels 1-2)                   | None                                                                    | Parsed-only   |
| CMD      | 2            | 2            | CLI       | Debug commands+options that are issued (levels 1-2)    | None                                                                    | Parsed-only   |
| DEL      | 3            | 3            | REC       | Debug delete actions (levels 1-3)                      | `engine/src/local_copy/debug_del.rs`, target `rsync::del`, 6 emit sites | Wired         |
| DELTASUM | 4            | 4            | SND+REC   | Debug delta-transfer checksumming (levels 1-4)         | `engine/src/local_copy/debug_deltasum/{checksum,matching,tracer}.rs`, target `rsync::deltasum`, 9 emit sites | Wired |
| DUP      | 1            | u8::MAX      | REC       | Debug weeding of duplicate names                       | None                                                                    | Parsed-only   |
| EXIT     | 3            | 3            | CLI+SRV   | Debug exit events (levels 1-3)                         | None                                                                    | Parsed-only   |
| FILTER   | 3            | 3            | SND+REC   | Debug filter actions (levels 1-3)                      | `filters/src/debug_filter.rs`, target `rsync::filter`                   | Wired         |
| FLIST    | 4            | 4            | SND+REC   | Debug file-list operations (levels 1-4)                | `engine/src/local_copy/debug_flist.rs`, target `rsync::flist`, 6 emit sites | Wired      |
| FUZZY    | 2            | 2            | REC       | Debug fuzzy scoring (levels 1-2)                       | None                                                                    | Parsed-only   |
| GENR     | 1            | u8::MAX      | REC       | Debug generator functions                              | Bridged via `target_to_debug_flag` aliasing `rsync::generator`          | Parsed-only (alias only) |
| HASH     | 1            | u8::MAX      | SND+REC   | Debug hashtable code                                   | None                                                                    | Parsed-only   |
| HLINK    | 3            | 3            | SND+REC   | Debug hard-link actions (levels 1-3)                   | None                                                                    | Parsed-only   |
| ICONV    | 2            | 2            | CLI+SRV   | Debug iconv character conversions (levels 1-2)         | None                                                                    | Parsed-only   |
| IO       | 4            | 4            | CLI+SRV   | Debug I/O routines (levels 1-4)                        | `protocol/src/debug_io.rs`, `fast_io/src/debug_io.rs`, `rsync_io/src/debug_io.rs`, target `rsync::io`, ~30 emit sites | Wired |
| NSTR     | 2            | u8::MAX      | CLI+SRV   | Debug negotiation strings                              | None                                                                    | Parsed-only   |
| OWN      | 2            | 2            | REC       | Debug ownership changes in users & groups (levels 1-2) | None                                                                    | Parsed-only   |
| PROTO    | 1            | u8::MAX      | CLI+SRV   | Debug protocol information                             | `protocol/src/debug_trace.rs` plus `target = "rsync::protocol"` events bridged to `DebugFlag::Proto` | Wired |
| RECV     | 1            | u8::MAX      | REC       | Debug receiver functions                               | `engine/src/local_copy/debug_recv/{trace_functions,tracer}.rs`, target `rsync::recv`, 9 emit sites | Wired |
| SEND     | 1            | u8::MAX      | SND       | Debug sender functions                                 | `engine/src/local_copy/debug_send.rs`, target `rsync::send`, 9 emit sites | Wired      |
| TIME     | 2            | 2            | REC       | Debug setting of modified times (levels 1-2)           | None                                                                    | Parsed-only   |

The 24-entry table matches `COUNT_DEBUG = DEBUG_TIME + 1` in
`rsync.h`. There is no separate `TIME2`/`DELTASUM2`/`OWN2` constant -
upstream uses `<flag><digit>` tokens to set per-flag levels; the same
entry in `debug_words[]` services every level for the flag.

## Pseudo-flags and prefixes

| Token              | Upstream behaviour (options.c:427-471)                | oc-rsync behaviour                                                                              | Parity |
|--------------------|-------------------------------------------------------|-------------------------------------------------------------------------------------------------|--------|
| `help`             | `output_item_help`, `exit_cleanup(0)`                 | `settings.help_requested = true`; CLI prints `DEBUG_HELP_TEXT` and exits cleanly                | Yes    |
| `ALL` / `all`      | Sets every flag to level 1                            | `enable_all` sets every flag to `Some(1)`                                                       | Yes    |
| `ALL2`..`ALL4`     | Sets every flag to N (clamped to 4)                   | Falls through `apply_flag_and_level` and is rejected as unknown token                           | Gap    |
| `1`                | Treated as `ALL1`                                     | `enable_all` (level 1)                                                                          | Yes    |
| `0`                | Treated as `NONE`                                     | `disable_all`                                                                                   | Yes    |
| `NONE` / `none`    | Zeroes every flag                                     | `disable_all` sets every flag to `Some(0)`                                                      | Yes    |
| `NONE0`            | Same as `NONE`                                        | Falls through `apply_flag_and_level` and is rejected as unknown token                           | Gap    |
| `no<flag>` / `-<flag>` | Not recognised by `parse_output_words`            | `parse_flag_and_level` strips `no`/`-` and forces level 0 (oc-rsync extension)                  | Extension |
| `<flag><digit>`    | Trailing digits parse as level, clamp at `MAX_OUT_LEVEL` | `parse_flag_and_level` trims trailing digits and parses suffix as `u8`; per-flag arm in `apply` enforces the documented per-flag cap | Yes (with caveats below) |
| Unknown token      | `Unknown --debug item: "<tok>"`, exit 1               | `invalid --debug flag '<tok>': use --debug=help for supported flags`, exit 1                    | Functional parity (different wording) |

## `-v` ladder

Upstream `set_output_verbosity` (options.c:513) walks
`debug_verbosity[]` from row `0` up to the current `-v` count and
applies each row at `DEFAULT_PRIORITY = 0`:

| `-v` count | Added debug levels (upstream `debug_verbosity[]`)                                                |
|:----------:|---------------------------------------------------------------------------------------------------|
| 0          | (none)                                                                                            |
| 1          | (none)                                                                                            |
| 2          | `BIND, CMD, CONNECT, DEL, DELTASUM, DUP, FILTER, FLIST, ICONV` (each at level 1)                  |
| 3          | `ACL, BACKUP, CONNECT2, DELTASUM2, DEL2, EXIT, FILTER2, FLIST2, FUZZY, GENR, OWN, RECV, SEND, TIME` |
| 4          | `CMD2, DELTASUM3, DEL3, EXIT2, FLIST3, ICONV2, OWN2, PROTO, TIME2`                                |
| 5          | `CHDIR, DELTASUM4, FLIST4, FUZZY2, HASH, HLINK`                                                   |

`VerbosityConfig::from_verbose_level` (`crates/logging/src/config.rs`)
is a hand-rolled cumulative table that mirrors the rows literally.
The match is exhaustive for `0..=5` and treats any level above 5 as
5, matching `MAX_VERBOSITY = 5`. `--debug=FLAG` overrides happen
after `from_verbose_level`; `USER_PRIORITY = 2` always beats
`DEFAULT_PRIORITY = 0` upstream and oc-rsync layers user overrides on
top last-write-wins.

## Tracing-target bridge

`crates/logging/src/tracing_bridge.rs::target_to_debug_flag` maps each
`tracing` target to a `DebugFlag` via `::<short>` or `::<long>`
substring match plus an exact-word fallback, with the deltasum/del
ordering chosen so `::deltasum` does not collide with `::del`. The
`generator` and `protocol` targets are accepted as aliases for `genr`
and `proto` respectively. `level_to_verbosity_level` maps
`tracing::Level` to verbosity level: `ERROR`/`WARN`/`INFO -> 1`,
`DEBUG -> 2`, `TRACE -> 3`. The bridge consults `debug_gte` before
emitting, so a flag must be enabled at the requested level.

## Gaps and missing implementations

1. **`ALL<N>` and `NONE0` not handled** - upstream accepts
   `--debug=ALL2` and clamps to 4. oc-rsync rejects these as unknown
   tokens. Fix: extend
   `DebugFlagSettings::apply` to detect `all<digit>` and `none<digit>`
   before falling through to `parse_flag_and_level`, then call
   `enable_all_to_level(N)`/`disable_all`. Cap `N` at 4.
2. **Per-flag cap missing for upstream-max-1 flags** - `acl`, `bind`,
   `chdir`, `dup`, `genr`, `hash`, `nstr`, `proto`, `recv`, `send`
   accept any `u8` value because no `if level > N` guard exists in
   `apply`. Upstream silently clamps to `MAX_OUT_LEVEL = 4`. Fix:
   add `if level > 4` guards to those arms (or a single shared cap)
   to match upstream behaviour.
3. **No `limit_output_verbosity` clamp** - upstream
   `options.c:527` invokes `limit_output_verbosity` during
   server-side option exchange to cap a peer's user-supplied level
   at the implicit `-v` ceiling. oc-rsync has no equivalent on the
   receiving end of wire negotiation; user-supplied levels are
   honoured verbatim regardless of peer `-v`.
4. **Sixteen flags Parsed-only** - `ACL`, `BACKUP`, `BIND`, `CHDIR`,
   `CMD`, `CONNECT`, `DUP`, `EXIT`, `FUZZY`, `GENR`, `HASH`, `HLINK`,
   `ICONV`, `NSTR`, `OWN`, `TIME` are wired through the parser, the
   bridge target table, and `DebugLevels::set`, but no producer
   emits on those targets. Each one needs a `debug_<flag>.rs`
   submodule under the owning crate plus `tracing::trace!` /
   `debug!` calls at the upstream `DEBUG_GTE` sites. Owning crates
   per side bit:
   - `metadata` (ACL, OWN, TIME, HLINK)
   - `engine` (BACKUP, DUP, FUZZY, GENR, EXIT receiver path)
   - `daemon`/`transport` (BIND, CONNECT, CMD, EXIT cli/srv path,
     ICONV, NSTR, CHDIR)
   - `checksums` (HASH)
5. **`generator` alias one-way** - the bridge maps
   `target = "rsync::generator"` to `DebugFlag::Genr` but no producer
   emits on either target, so the alias is parser-side only. Add a
   `debug_genr.rs` to `engine/src/local_copy/` that emits on
   `target = "rsync::genr"` to match upstream
   `--debug=GENR` output.
6. **`info_verbosity` mirror not in scope here** - upstream's
   `info_words[]` table is the `--info=FLAG` cousin of
   `debug_words[]`. Coverage parity for `--info` should be tracked
   separately; this audit deliberately stops at `--debug`.

## Summary

- 24/24 upstream `--debug` flags parse cleanly. `help`, `ALL`,
  `NONE`, numeric suffixes, and the oc-rsync `no<flag>`/`-<flag>`
  extensions all route correctly.
- 8 flags are Wired with at least one production gating site:
  `DEL`, `DELTASUM`, `FILTER`, `FLIST`, `IO`, `PROTO`, `RECV`,
  `SEND`. The remaining 16 are Parsed-only.
- Per-flag caps match upstream documentation for every flag whose
  upstream max is greater than 1. Flags with upstream max of 1
  currently accept any `u8` value; upstream clamps to 4.
- `-v` ladder mirrors `debug_verbosity[]` row-for-row up to level 5.
- Two parser gaps (`ALL<N>`, `NONE0`) and one wire negotiation gap
  (`limit_output_verbosity`) remain.
