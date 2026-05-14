# `--debug=FLAGS` verbosity matrix vs upstream rsync 3.4.1

Tracking issue: #2113. Last verified: 2026-05-14 against `origin/master`.

Companion audits in the output-family series:

- `docs/audits/progress-line-format.md` (#2110) - byte-for-byte progress
  line format.
- `docs/audits/info-flags-matrix.md` (#2112) - `--info=FLAGS` family,
  parallel parser and gating tree.
- `docs/audits/debug-flags-audit.md` and
  `docs/audits/debug-flags-matrix.md` - prior `--debug=FLAGS` audits;
  this file is the canonical verbosity matrix with per-producer
  evidence and the gap roll-up.

`--info=FLAGS` (#2112) and `--debug=FLAGS` are parallel option families
upstream: shared `parse_output_words` parser, shared `output_item_help`
formatter, separate per-call-site gating macros (`INFO_GTE` vs
`DEBUG_GTE`). The audit cross-checks the two but is otherwise scoped to
`--debug`.

Sources:

- Upstream tables and parser:
  `target/interop/upstream-src/rsync-3.4.1/options.c:228-235`
  (`debug_verbosity[]`), `:245` (`MAX_OUT_LEVEL`),
  `:247` (`debug_levels[COUNT_DEBUG]`),
  `:249-252` (priority bands),
  `:254-257` (W_* side bits),
  `:259-266` (`output_struct`),
  `:287-315` (`debug_words[]`),
  `:340-425` (`make_output_option`),
  `:427-471` (`parse_output_words`),
  `:473-510` (`output_item_help`),
  `:512-524` (`set_output_verbosity`),
  `:527-553` (`limit_output_verbosity`),
  `:555-578` (`reset_output_levels`/`negate_output_levels`).
- Upstream constants:
  `target/interop/upstream-src/rsync-3.4.1/rsync.h:1414-1462`
  (`DEBUG_GTE`, `DEBUG_*` index constants, `COUNT_DEBUG`).
- oc-rsync per-flag storage:
  `crates/logging/src/levels/debug.rs:14-66` (`DebugFlag`),
  `:74-125` (`DebugLevels`), `:127-215` (`get`/`set`/`set_all`).
- oc-rsync verbosity stack:
  `crates/logging/src/config.rs:27-32` (`VerbosityConfig`),
  `:43-195` (`from_verbose_level`),
  `:203-225` (`apply_info_flag`),
  `:233-266` (`apply_debug_flag`),
  `:273-...` (`parse_flag_token`).
- oc-rsync CLI front end:
  `crates/cli/src/frontend/execution/flags/debug.rs:10-36`
  (`DebugFlagSettings`), `:71-123` (`enable_all`/`disable_all`),
  `:125-281` (`apply` per-flag cap arms),
  `:285-310` (`parse_flag_and_level`),
  `:326-352` (`parse_debug_flags`),
  `:354-384` (`DEBUG_HELP_TEXT`).
- oc-rsync tracing bridge:
  `crates/logging/src/tracing_bridge.rs:80-158`
  (`target_to_debug_flag`), `:161-169` (`level_to_verbosity_level`),
  `:176-206` (`Layer::on_event`).
- oc-rsync gating macro:
  `crates/logging/src/macros.rs:69-76` (`debug_log!`).

## 1. Upstream level model

Upstream defines four priority bands and two hard caps
(`options.c:245-252`):

- `MAX_OUT_LEVEL = 4` (`options.c:245`). Any trailing-digit suffix is
  clamped to 4 by `parse_output_words` (`options.c:444-445`); level 0
  disables.
- `MAX_VERBOSITY = sizeof debug_verbosity / sizeof debug_verbosity[0] - 1
  = 5` (`options.c:237`). Bounds the cumulative `-v` ladder; values
  above 5 are clamped in `set_output_verbosity`
  (`options.c:517-518`).
- `DEFAULT_PRIORITY = 0`, `HELP_PRIORITY = 1`, `USER_PRIORITY = 2`,
  `LIMIT_PRIORITY = 3` (`options.c:249-252`). User-supplied
  `--debug=FOO` (priority 2) always wins over `-v` defaults
  (priority 0). `LIMIT_PRIORITY` is reserved for the server-side
  clamp in `limit_output_verbosity` (`options.c:540-541`).

The `where` field of each `output_struct` row records which roles
(`W_CLI`, `W_SRV`, `W_SND`, `W_REC`) legitimately emit the flag
(`options.c:254-257`). These bits are not enforced by the parser but
are consulted by `make_output_option` to filter the option string sent
to the peer (`options.c:360-365`, `:409-410`).

## 2. Per-flag matrix

The **upstream max** column is the largest `N` ever passed to
`DEBUG_GTE(FLAG, N)` in the upstream tree (so it bounds the useful
range; upstream silently clamps user-supplied levels at
`MAX_OUT_LEVEL = 4` regardless). The **oc-rsync cap** column is the
level cap enforced in
`crates/cli/src/frontend/execution/flags/debug.rs::apply` - tokens
exceeding the cap return `invalid --debug flag '<tok>': use
--debug=help for supported flags` (`flags/debug.rs:317-322`). The
**oc-rsync status** column is one of:

- **impl** - parser accepts the flag, at least one production
  `debug_log!(<Flag>, _, _)` call exists outside `tests/`, and the
  flag's `target = "rsync::<flag>"` traffic is bridged to
  `DebugFlag::<Flag>` in `target_to_debug_flag`.
- **partial** - some emit sites exist but cover a strict subset of
  the upstream level range, or only one of the two upstream
  call-site clusters has been ported.
- **missing** - parser accepts the flag and the bridge maps the
  target, but no production producer calls `debug_log!` or
  `tracing::*(target = "rsync::<flag>", ...)` outside `tests/` and
  `examples/`.

Producer counts come from
`grep -rn 'debug_log!(<Flag>,' crates/ --include='*.rs' | grep -v
'tests\|examples\|src/macros\|src/lib.rs'`, run on
`origin/master @ 4d166e041`.

| Flag       | Upstream `DEBUG_GTE` max | oc-rsync cap | Side bits | Upstream `debug_words[]` help (verbatim) | Production producers | Status |
|------------|:------------------------:|:------------:|-----------|------------------------------------------|----------------------|--------|
| ACL        | 1 | u8::MAX | W_SND\|W_REC | Debug extra ACL info | none | missing |
| BACKUP     | 1 (help says 1-2) | 2 | W_REC | Debug backup actions (levels 1-2) | none | missing |
| BIND       | 1 | u8::MAX | W_CLI | Debug socket bind actions | none | missing |
| CHDIR      | 1 | u8::MAX | W_CLI\|W_SRV | Debug when the current directory changes | none | missing |
| CMD        | 2 | 2 | W_CLI | Debug commands+options that are issued (levels 1-2) | none | missing |
| CONNECT    | 2 | 2 | W_CLI | Debug connection events (levels 1-2) | `crates/rsync_io/src/ssh/connection.rs`, `crates/rsync_io/src/ssh/builder.rs`, `crates/rsync_io/src/daemon/negotiate.rs`, `crates/rsync_io/src/binary/negotiate.rs`, `crates/rsync_io/src/negotiation/sniffer.rs`, `crates/rsync_io/src/session/handshake/negotiate.rs` (7 sites at levels 1-3) | partial (emits level 3 not in upstream range) |
| DEL        | 3 | 3 | W_REC | Debug delete actions (levels 1-3) | `crates/transfer/src/receiver/directory/deletion.rs:179` (1 site at level 1) | partial (level 1 only) |
| DELTASUM   | 4 | 4 | W_SND\|W_REC | Debug delta-transfer checksumming (levels 1-4) | `crates/match/src/generator.rs:404`, `crates/transfer/src/delta_apply/applicator.rs:441` (2 sites at level 2) | partial (level 2 only) |
| DUP        | 1 | u8::MAX | W_REC | Debug weeding of duplicate names | `crates/flist/src/file_list_walker.rs:85` (1 site at level 1) | impl |
| EXIT       | 3 | 3 | W_CLI\|W_SRV | Debug exit events (levels 1-3) | `crates/transfer/src/temp_cleanup.rs:147` (1 site at level 2) | partial (level 2 only) |
| FILTER     | 3 | 3 | W_SND\|W_REC | Debug filter actions (levels 1-3) | `crates/filters/src/decision.rs:69,71`, `crates/filters/src/compiled/rule.rs:43,60,68,74` (5 sites covering levels 1-3) | impl |
| FLIST      | 4 | 4 | W_SND\|W_REC | Debug file-list operations (levels 1-4) | `crates/flist/src/file_list_walker.rs:32,89,101,119,130,241`, `crates/transfer/src/generator/file_list/{inc_recurse.rs:46,mod.rs:103,219}`, `crates/transfer/src/generator/transfer.rs:188,202,212,217`, `crates/transfer/src/receiver/file_list.rs:154,215,226`, `crates/protocol/src/flist/sort.rs:208,375`, `crates/protocol/src/flist/incremental/{ready_entry.rs,mod.rs}`, `crates/protocol/src/flist/read/{flags.rs,metadata.rs,mod.rs,name.rs}` (23 sites covering levels 1-4) | impl |
| FUZZY      | 2 | 2 | W_REC | Debug fuzzy scoring (levels 1-2) | none | missing |
| GENR       | 1 | u8::MAX | W_REC | Debug generator functions | none on `rsync::genr`; bridge accepts `rsync::generator` alias (`tracing_bridge.rs:113-119`) but no producer emits on either target | missing |
| HASH       | 1 | u8::MAX | W_SND\|W_REC | Debug hashtable code | none | missing |
| HLINK      | 3 (help says 1-3) | 3 | W_SND\|W_REC | Debug hard-link actions (levels 1-3) | none | missing |
| ICONV      | 2 | 2 | W_CLI\|W_SRV | Debug iconv character conversions (levels 1-2) | none | missing |
| IO         | 4 | 4 | W_CLI\|W_SRV | Debug I/O routines (levels 1-4) | `crates/transfer/src/disk_commit/thread.rs:117,125,132,149,153,155` plus `tracing::*(target: "rsync::io", ...)` in `crates/protocol/src/debug_io.rs`, `crates/fast_io/src/debug_io.rs`, `crates/rsync_io/src/debug_io.rs` (`debug_io.rs` trace funcs not called from production - see gap G3) | partial (levels 1 and 3 emitted via `debug_log!`; trace-func helpers unwired) |
| NSTR       | 2 | u8::MAX | W_CLI\|W_SRV | Debug negotiation strings | `crates/protocol/src/negotiation/capabilities/negotiate.rs` (6 sites covering levels 1-3, upstream-verbatim wording from `compat.c:215,373-378,521-525,866`) | impl |
| OWN        | 2 | 2 | W_REC | Debug ownership changes in users & groups (levels 1-2) | none | missing |
| PROTO      | 1 | u8::MAX | W_CLI\|W_SRV | Debug protocol information | `crates/protocol/src/negotiation/capabilities/negotiate.rs`, `crates/protocol/src/multiplex/io/send.rs`, `crates/protocol/src/multiplex/io/recv.rs` (6 sites at levels 1-2) | partial (level 2 not in upstream range) |
| RECV       | 1 | u8::MAX | W_REC | Debug receiver functions | `crates/transfer/src/receiver/directory/links.rs:264` (1 site at level 2) | partial (level 2 not in upstream range) |
| SEND       | 1 | u8::MAX | W_SND | Debug sender functions | `crates/transfer/src/generator/transfer.rs` (5 sites at level 1, upstream-verbatim wording from `sender.c:217,254,277,445,457`); `crates/engine/src/local_copy/debug_send.rs` trace helpers remain unwired (see gap G3) | impl (D13 RESOLVED) |
| TIME       | 2 | 2 | W_REC | Debug setting of modified times (levels 1-2) | `crates/transfer/src/disk_commit/thread.rs` (3 sites at levels 1-2) | impl |

Total: 24 upstream `DEBUG_*` flags, matching `COUNT_DEBUG = DEBUG_TIME + 1`
(`rsync.h:1462`). Status roll-up:

- **impl**: 6 - DUP, FILTER, FLIST, NSTR, SEND, TIME.
- **partial**: 7 - CONNECT, DEL, DELTASUM, EXIT, IO, PROTO, RECV.
- **missing**: 11 - ACL, BACKUP, BIND, CHDIR, CMD, FUZZY, GENR, HASH,
  HLINK, ICONV, OWN.

`SEND` (D13) is now wired into the generator's send loop with the
five upstream-verbatim messages from `sender.c send_files` (lines
217, 254, 277, 445, 457). The `crates/engine/src/local_copy/debug_send.rs`
trace helpers remain unwired; production emissions go through
`debug_log!(Send, 1, ...)` directly. The same `debug_log!`-vs-trace-helper
pattern holds for `crates/engine/src/local_copy/debug_del.rs`,
`crates/engine/src/local_copy/debug_recv/trace_functions.rs`,
`crates/engine/src/local_copy/debug_deltasum/{checksum,matching}.rs`,
and the three `debug_io.rs` modules - the trace functions exist and
target the right `rsync::*` namespace, but the production code emits
via direct `debug_log!` calls instead and many subsystems do not emit
at all.

## 3. Pseudo-flags and prefixes

`parse_output_words` (`options.c:427-471`) applies the pseudo-flag
tokens before consulting the `debug_words[]` lookup:

| Token              | Upstream behaviour | oc-rsync behaviour | Parity |
|--------------------|--------------------|--------------------|--------|
| `help`             | `output_item_help`, then `exit_cleanup(0)` (`options.c:446-449`) | `parse_debug_flags` sets `settings.help_requested = true` (`flags/debug.rs:343-345`); the drive layer prints `DEBUG_HELP_TEXT` (`flags/debug.rs:354-384`) before exiting 0. | yes (different help layout - see section 5) |
| `ALL` / `all`      | `len = 0` after `strncasecmp(..., "all", 3)`; loop body assigns level 1 (or trailing digit if present) to every entry (`options.c:452-453`, `:454-463`) | `apply` recognises bare `all`/`1` and calls `enable_all()` (`flags/debug.rs:128-131`) | yes |
| `ALL2` ... `ALL4`  | Trailing digit parsed as level, clamped to 4 (`options.c:443-445`); every entry set to that level (`options.c:454-460`) | Falls through `parse_flag_and_level` (`flags/debug.rs:291-310`), which trims trailing digits but rejects `all` because `KNOWN_FLAGS` does not include it; the per-flag arm in `apply` returns `invalid --debug flag` (`flags/debug.rs:279`) | **gap (G1)** |
| `1`                | Parsed as `lev = 1, len = 0` (`options.c:439-443`), behaves like `ALL` (level 1) | `apply` recognises bare `1` and calls `enable_all()` (`flags/debug.rs:128-131`) | yes |
| `0`                | `len = 0, lev = 0`, zeroes every flag (`options.c:454-463`) | `apply` recognises bare `0` and calls `disable_all()` (`flags/debug.rs:133-136`) | yes |
| `NONE` / `none`    | `len = lev = 0`; loop body zeroes every entry (`options.c:450-451`, `:454-463`) | `apply` recognises bare `none`/`0` and calls `disable_all()` (`flags/debug.rs:133-136`) | yes |
| `NONE0`            | Trailing `0` accepted, equivalent to `NONE` (`options.c:443-445`, `:450-451`) | Falls through `parse_flag_and_level`; `none` not in `KNOWN_FLAGS`, rejected | **gap (G1)** |
| `no<flag>` / `-<flag>` | Not recognised; upstream `parse_output_words` does not strip these prefixes. The token reaches the loop, fails the `strncasecmp` against every `debug_words[].name`, and falls into the `Unknown --debug item` error at `options.c:465-469`. | `parse_flag_and_level` strips `no` or `-` prefix and forces level 0 (`flags/debug.rs:303-307`); the per-flag arm then sets the flag to 0 | **extension** (oc-rsync accepts, upstream rejects; documented in `DEBUG_HELP_TEXT` at `flags/debug.rs:382`) |
| `<flag><digit>`    | Trailing digits parsed as level; clamped at `MAX_OUT_LEVEL = 4` (`options.c:443-445`) | `parse_flag_and_level` trims trailing digits, validates the base against `KNOWN_FLAGS`, and parses the suffix as `u8` (`flags/debug.rs:292-298`); the per-flag arm in `apply` enforces the per-flag cap | yes for flags whose upstream max is documented; **gap (G2)** for flags whose upstream max is 1 (`acl`, `bind`, `chdir`, `dup`, `genr`, `hash`, `nstr`, `proto`, `recv`, `send`) - oc-rsync accepts any `u8` value while upstream silently clamps to 4. |
| Unknown token      | `Unknown --debug item: "<tok>"`, exit 1 (`options.c:465-469`) | `invalid --debug flag '<tok>': use --debug=help for supported flags`, exit 1 (`flags/debug.rs:317-322`) | functional parity (different wording) |

## 4. `-v` cumulative ladder

`set_output_verbosity` (`options.c:512-524`) walks `debug_verbosity[]`
from row `0` up to the current `-v` count and applies each row at
`DEFAULT_PRIORITY = 0`. The rows are (`options.c:228-235`):

| `-v` count | Added debug levels (upstream `debug_verbosity[]`) |
|:----------:|----------------------------------------------------|
| 0 | (none - `debug_verbosity[0] = NULL`) |
| 1 | (none - `debug_verbosity[1] = NULL`) |
| 2 | `BIND, CMD, CONNECT, DEL, DELTASUM, DUP, FILTER, FLIST, ICONV` (each level 1) |
| 3 | `ACL, BACKUP, CONNECT2, DELTASUM2, DEL2, EXIT, FILTER2, FLIST2, FUZZY, GENR, OWN, RECV, SEND, TIME` |
| 4 | `CMD2, DELTASUM3, DEL3, EXIT2, FLIST3, ICONV2, OWN2, PROTO, TIME2` |
| 5 | `CHDIR, DELTASUM4, FLIST4, FUZZY2, HASH, HLINK` |

oc-rsync mirrors the table literally in
`VerbosityConfig::from_verbose_level`
(`crates/logging/src/config.rs:43-195`). The match is exhaustive for
`0..=5` and treats any level above 5 as 5 (`config.rs:154-191`), matching
upstream's `if (level > MAX_VERBOSITY) level = MAX_VERBOSITY;` clamp
(`options.c:517-518`).

User-supplied `--debug=FLAG` overrides happen after the ladder is
applied because the CLI driver invokes `parse_debug_flags` and then
mutates the verbosity config last; this is equivalent to upstream's
priority-based override (`USER_PRIORITY = 2` beats
`DEFAULT_PRIORITY = 0`) in the common case (`config.rs:233-266`).

## 5. `--debug=help` output

Upstream prints the table built by `output_item_help`
(`options.c:473-510`). It iterates `debug_words[]` (`options.c:485-486`),
then prints the synthetic `ALL`/`NONE`/`HELP` rows
(`options.c:489-497`), then a per-verbosity summary block
(`options.c:499-509`) constructed by re-running `parse_output_words`
with `HELP_PRIORITY` against each `debug_verbosity[j]` row and
formatting the resulting `levels[]` array with `make_output_option`.

oc-rsync prints a fixed string at `DEBUG_HELP_TEXT`
(`flags/debug.rs:354-384`). Divergences from upstream:

1. The synthetic `ALL`/`NONE`/`HELP` table includes `ALL` and `none`
   but not the level-suffix form (`ALL2`, `NONE0`) - matches the gap
   in section 3.
2. The per-verbosity summary block (`0)`, `1)`, ..., `5)`) is not
   printed; users cannot discover which flags `-vvv` would enable
   without consulting the source or `man rsync`.
3. The leading line `Use OPT or OPT1 for level 1 output, OPT2 for
   level 2, etc.; OPT0 silences.` from `options.c:483` is omitted;
   the trailing paragraph documents the `no`/`-` prefix and level
   suffix forms instead (`flags/debug.rs:382-384`).
4. The flag rows use a fixed `<NAME>    <help>` layout with 12-column
   name padding; upstream uses `"%-10s %s\n"` (`options.c:478`,
   `:486`). The width difference is cosmetic but observable when
   comparing `--debug=help` output byte-for-byte.

## 6. `make_output_option` and remote forwarding

Upstream's `make_output_option` (`options.c:340-425`) constructs the
`--debug=...` token forwarded to the peer, deduplicating implied
flags and abbreviating dense level assignments to `ALL<N>`. The
function inspects each row's `priority` field to skip
`DEFAULT_PRIORITY` entries (which the peer will derive from its own
`-v`) and the `where` mask to restrict the option to roles that
actually need the flag.

oc-rsync has no equivalent of `make_output_option`. Remote
invocations pass `--debug=FLAGS` through verbatim (whatever the user
typed on the CLI), so:

- Implied `-v` flags are not forwarded - matches upstream by accident
  because the peer applies its own `-v` ladder.
- User-set flags are not deduplicated; the peer receives the full
  comma-separated list as typed.
- Server-only flags (`W_SRV`-only entries) are not stripped before
  forwarding; the peer receives flags it cannot act on.

## 7. `limit_output_verbosity` clamping

Upstream `limit_output_verbosity` (`options.c:527-553`) is invoked
during server-side option exchange (`options.c:2929` in
`server_options`) to cap a peer's user-supplied level at the implicit
`-v` ceiling. Without this clamp, a client could ask the server for
`--debug=DELTASUM4` even when the server is running at `-v0`; the
upstream behaviour silently lowers `DELTASUM` to 0 on the server
side.

oc-rsync has no equivalent clamp on the receiving end of wire
negotiation. User-supplied levels are honoured verbatim regardless of
peer verbosity. This is a parity gap (G4) but not a security or
correctness issue - it only affects how verbose the server's
diagnostic output becomes.

## 8. Tracing-target bridge

`target_to_debug_flag` (`tracing_bridge.rs:80-158`) maps each
`tracing` target to a `DebugFlag` via `::<short>` or `::<long>`
substring match plus an exact-word fallback. The `deltasum`/`del`
ordering at `:92-101` is deliberate so `::deltasum` does not collide
with `::del`. Aliases:

- `::delta` -> `DebugFlag::Deltasum` (`:93,95`)
- `::delete` -> `DebugFlag::Del` (`:99-100`)
- `::file_list` -> `DebugFlag::Flist` (`:106,108`)
- `::generator` -> `DebugFlag::Genr` (`:114,116`)
- `::hardlink` -> `DebugFlag::Hlink` (`:122,124`)
- `::ownership` -> `DebugFlag::Own` (`:132,134`)
- `::protocol` -> `DebugFlag::Proto` (`:139,141`)
- `::receiver` -> `DebugFlag::Recv` (`:146,148`)
- `::sender` -> `DebugFlag::Send` (`:152`)

`level_to_verbosity_level` (`tracing_bridge.rs:161-169`) maps
`tracing::Level` to verbosity level: `ERROR`/`WARN`/`INFO -> 1`,
`DEBUG -> 2`, `TRACE -> 3`. The bridge consults `debug_gte` before
emitting (`tracing_bridge.rs:184`), so a flag must be enabled at the
requested level before the event materialises.

## 9. Gap enumeration

| ID | Severity | Status | Description |
|----|----------|--------|-------------|
| G1 | Low | open | `ALL<N>` and `NONE0` syntactic forms are rejected as unknown tokens (`flags/debug.rs:279`). Upstream accepts them (`options.c:443-445`, `:452-453`, `:450-451`) and applies the trailing digit. Fix: extend `DebugFlagSettings::apply` to detect `all<digit>` and `none<digit>` before falling through to `parse_flag_and_level`, then call `enable_all_to_level(N)` / `disable_all()`. Cap N at 4 to match `MAX_OUT_LEVEL`. |
| G2 | Low | open | Per-flag cap missing for upstream-max-1 flags. `acl`, `bind`, `chdir`, `dup`, `genr`, `hash`, `nstr`, `proto`, `recv`, `send` accept any `u8` value (no `if level > N` guard in `flags/debug.rs::apply`). Upstream silently clamps to `MAX_OUT_LEVEL = 4`. Fix: add `if level > 4 { return Err(debug_flag_error(display)); }` guards on those arms, or a single shared cap before the per-flag dispatch. |
| G3 | High | open | 13 flags have no production producer (ACL, BACKUP, BIND, CHDIR, CMD, FUZZY, GENR, HASH, HLINK, ICONV, NSTR, OWN, SEND). Of these, SEND has a complete subsystem module (`debug_send.rs`) whose public trace helpers are never called from production code paths; only its own unit tests invoke them. Owning crates for the missing producers: `metadata` (ACL, OWN, HLINK), `engine` (BACKUP, FUZZY, GENR), `transfer`/`rsync_io` (BIND, CMD, ICONV, NSTR), `checksums` (HASH), `core` (CHDIR), `engine`/`transfer` (SEND wiring). |
| G4 | Low | open | `limit_output_verbosity` (upstream `options.c:527-553`) is not implemented. User-supplied per-flag levels are not clamped to the peer's `-v` ceiling during option exchange. Mitigation: implement on `server_options` parsing path and re-run the ladder with `LIMIT_PRIORITY` to compute the cap. |
| G5 | Low | open | `make_output_option` (upstream `options.c:340-425`) is not implemented. Remote command forwarding of user-priority debug flags relies on raw passthrough rather than upstream's deduplicated `--debug=ALL2,NONREG0,...` style. User-visible effect is limited to the exact command string the peer sees in `ps` output. |
| G6 | Low | open | `--debug=help` text omits the per-verbosity summary block (`0)`, `1)`, ..., `5)`) that upstream's `output_item_help` prints at `options.c:499-509`. Cosmetic but useful for discovery. |
| G7 | Medium | open | Partial-level coverage: `DEL`, `DELTASUM`, `EXIT` emit at a single level rather than spanning the upstream range (1-3 for DEL/EXIT, 1-4 for DELTASUM). `IO` emits at levels 1 and 3 but skips 2 and 4. `CONNECT`, `PROTO`, `RECV` emit at levels not in the upstream range (CONNECT 3, PROTO 2, RECV 2). Fix: align the emission level numbers with upstream by walking the `DEBUG_GTE(<FLAG>, N)` call sites in the upstream tree and ensuring oc-rsync emits at each N. |
| G8 | Low | open | `generator` -> `genr` alias in the bridge (`tracing_bridge.rs:113-119`) is one-way; no producer emits on either target. Wiring `rsync::genr` emissions from `crates/transfer/src/generator/*` would lift GENR to impl. |

## 10. Per-flag insertion plan for the missing producers

| Flag | Suggested insertion crate | Suggested call site |
|------|---------------------------|---------------------|
| ACL  | `crates/metadata/src/acl/` | After every ACL apply / read, on the `rsync::acl` target. Upstream emits in `acls.c:make_acl()` and `set_acl()`. |
| BACKUP | `crates/engine/src/local_copy/backup.rs` | Before each backup file rename. Upstream emits in `backup.c:make_bak_dir`. |
| BIND | `crates/rsync_io/src/socket/` | After `bind()` succeeds on the listener. Upstream emits in `socket.c:open_socket_in`. |
| CHDIR | `crates/core/src/client/path/` and `crates/daemon/src/jail/` | After every `chdir()` syscall. Upstream emits in `util1.c:do_chdir`. |
| CMD | `crates/cli/src/frontend/execution/drive/options.rs` | When `--server` argv is built and when SSH command line is constructed. Upstream emits in `options.c:server_options` and `pipe.c:do_cmd`. |
| FUZZY | `crates/transfer/src/generator/fuzzy.rs` (if present, else `match` crate) | When a fuzzy basis is selected. Upstream emits in `generator.c:find_fuzzy`. |
| GENR | `crates/transfer/src/generator/mod.rs` | At each phase boundary of the generator loop. Upstream emits in `generator.c:generate_files`. |
| HASH | `crates/checksums/src/strong/` and `crates/match/src/hashtable.rs` | At hashtable construction and lookup. Upstream emits in `match.c:build_hash_table`. |
| HLINK | `crates/transfer/src/receiver/directory/links.rs` (extend existing RECV emitter) | When a hardlink master/follower is resolved. Upstream emits in `hlink.c:hard_link_one`. |
| ICONV | `crates/protocol/src/iconv/` | When iconv conversion succeeds or fails. Upstream emits in `flist.c` near send/recv name paths. |
| NSTR | `crates/rsync_io/src/negotiation/` | During server/client capability string exchange. Upstream emits in `compat.c:set_allow_inc_recurse` and friends. |
| OWN | `crates/metadata/src/ownership/` | When uid/gid mapping is consulted. Upstream emits in `uidlist.c:map_uid` / `map_gid`. |
| SEND | Connect `crates/transfer/src/sender/file.rs` to the existing `crates/engine/src/local_copy/debug_send.rs::trace_send_file_start`/`trace_send_file_end` helpers. The subsystem is already implemented but unused. |

Each row is a follow-up task; this audit only enumerates them. The
PR for #2113 lands the audit only - no code changes.

## 11. Test action plan

Once gaps G1-G3 are addressed, add the following test fixtures:

- `tests/cli/debug_help.txt` - golden output of
  `oc-rsync --debug=help` to lock the table layout once it matches
  upstream `output_item_help`.
- `tests/cli/debug_all_levels.rs` - assert that `--debug=ALL2` and
  `--debug=NONE0` are accepted (G1) and that `--debug=ACL5`,
  `--debug=BIND9`, etc. are rejected with the per-flag cap error (G2).
- `tests/logging/v_ladder.rs` - parametric assertion that
  `VerbosityConfig::from_verbose_level(j)` produces the expected
  `DebugLevels` for `j = 0..=6` (mirroring `debug_verbosity[]`).
- `tests/logging/debug_emission.rs` - per-flag smoke test that drives
  a representative production path and asserts the corresponding
  `debug_log!` site fires when the flag is enabled. One assertion per
  flag once G3 producers are wired.

## 12. Upstream source references

- `options.c:228-235` - `debug_verbosity[]` cumulative table.
- `options.c:237` - `MAX_VERBOSITY` definition.
- `options.c:245` - `MAX_OUT_LEVEL` per-flag cap.
- `options.c:247` - `debug_levels[COUNT_DEBUG]` storage.
- `options.c:249-252` - priority bands (`DEFAULT`, `HELP`, `USER`,
  `LIMIT`).
- `options.c:254-257` - `W_CLI`/`W_SRV`/`W_SND`/`W_REC` masks.
- `options.c:259-266` - `output_struct` row layout.
- `options.c:287-315` - `debug_words[]` table.
- `options.c:340-425` - `make_output_option` server option
  serialiser.
- `options.c:427-471` - `parse_output_words` token parser.
- `options.c:473-510` - `output_item_help` help printer.
- `options.c:512-524` - `set_output_verbosity` ladder applier.
- `options.c:527-553` - `limit_output_verbosity` peer clamp.
- `options.c:555-578` - `reset_output_levels`,
  `negate_output_levels`.
- `rsync.h:1414-1462` - `DEBUG_GTE`/`DEBUG_EQ` macros and
  `DEBUG_ACL..DEBUG_TIME` index constants.

## 13. oc-rsync source references

- `crates/cli/src/frontend/execution/flags/debug.rs:10-36` -
  `DebugFlagSettings` storage.
- `crates/cli/src/frontend/execution/flags/debug.rs:71-123` -
  `enable_all` and `disable_all`.
- `crates/cli/src/frontend/execution/flags/debug.rs:125-281` -
  `apply` per-flag cap dispatch.
- `crates/cli/src/frontend/execution/flags/debug.rs:285-310` -
  `parse_flag_and_level` token splitter.
- `crates/cli/src/frontend/execution/flags/debug.rs:326-352` -
  `parse_debug_flags` entry point.
- `crates/cli/src/frontend/execution/flags/debug.rs:354-384` -
  `DEBUG_HELP_TEXT` constant.
- `crates/logging/src/levels/debug.rs:14-66` - `DebugFlag` enum.
- `crates/logging/src/levels/debug.rs:74-125` - `DebugLevels`
  storage.
- `crates/logging/src/levels/debug.rs:127-215` -
  `DebugLevels::{get, set, set_all}`.
- `crates/logging/src/config.rs:27-32` - `VerbosityConfig`.
- `crates/logging/src/config.rs:43-195` - `from_verbose_level`
  cumulative ladder.
- `crates/logging/src/config.rs:233-266` - `apply_debug_flag` per-flag
  override.
- `crates/logging/src/macros.rs:69-76` - `debug_log!` gating macro.
- `crates/logging/src/tracing_bridge.rs:80-158` - `target_to_debug_flag`.
- `crates/logging/src/tracing_bridge.rs:161-169` -
  `level_to_verbosity_level`.
- `crates/logging/src/tracing_bridge.rs:176-206` -
  `Layer::on_event` bridge core.
