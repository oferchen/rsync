# `--info=FLAGS` verbosity matrix audit vs upstream rsync 3.4.1

Tracking: oc-rsync task #2112. Companion to #2110 (progress-line format),
#2111 (stats-block format), and #2113 (debug-flags verbosity matrix).
Last verified: 2026-05-14 against `origin/master`.

## Scope

Compare the per-flag, per-level dispatch of `--info=FLAGS` between upstream
rsync 3.4.1 (`target/interop/upstream-src/rsync-3.4.1/`) and oc-rsync.
Downstream tools that grep on these messages depend on flag/level gating
being identical, so this audit catalogues every `INFO_*` flag, the maximum
level upstream actually consumes, and the corresponding oc-rsync wiring.

The audit only covers the gating semantics: the exact wording of each
emitted line is out of scope here, except where oc-rsync emits no message
at all for a level upstream does emit.

## 1. Upstream flag table

### 1.1 Flag definitions

`options.c:270-285` declares `info_words[]` with thirteen entries plus a
sentinel. The `INFO_WORD` macro keys each entry to the `INFO_*` enum in
`rsync.h:1421-1435`:

```c
static struct output_struct info_words[COUNT_INFO+1] = {
    INFO_WORD(BACKUP,   W_REC,         "Mention files backed up"),
    INFO_WORD(COPY,     W_REC,         "Mention files copied locally on the receiving side"),
    INFO_WORD(DEL,      W_REC,         "Mention deletions on the receiving side"),
    INFO_WORD(FLIST,    W_CLI,         "Mention file-list receiving/sending (levels 1-2)"),
    INFO_WORD(MISC,     W_SND|W_REC,   "Mention miscellaneous information (levels 1-2)"),
    INFO_WORD(MOUNT,    W_SND|W_REC,   "Mention mounts that were found or skipped"),
    INFO_WORD(NAME,     W_SND|W_REC,   "Mention 1) updated file/dir names, 2) unchanged names"),
    INFO_WORD(NONREG,   W_REC,         "Mention skipped non-regular files (default 1, 0 disables)"),
    INFO_WORD(PROGRESS, W_CLI,         "Mention 1) per-file progress or 2) total transfer progress"),
    INFO_WORD(REMOVE,   W_SND,         "Mention files removed on the sending side"),
    INFO_WORD(SKIP,     W_REC,         "Mention files skipped due to transfer overrides (levels 1-2)"),
    INFO_WORD(STATS,    W_CLI|W_SRV,   "Mention statistics at end of run (levels 1-3)"),
    INFO_WORD(SYMSAFE,  W_SND|W_REC,   "Mention symlinks that are unsafe"),
    { NULL, "--info", 0, 0, 0, 0 }
};
```

Per-flag and per-level enable bits live in the global `info_levels[]`
array (`options.c:247`). All checks go through the `INFO_GTE(flag, lvl)`
macro at `rsync.h:1416`: `info_levels[INFO_##flag] >= (lvl)`. The hard
cap is `MAX_OUT_LEVEL 4` (`options.c:245`); any user-supplied digit
above that is clamped in `parse_output_words()` at `options.c:444-445`.

### 1.2 Implicit verbosity ladder

`options.c:239-243` enumerates which info flags `-v`, `-vv`, ... raise:

```c
static const char *info_verbosity[1+MAX_VERBOSITY] = {
    /*0*/ "NONREG",
    /*1*/ "COPY,DEL,FLIST,MISC,NAME,STATS,SYMSAFE",
    /*2*/ "BACKUP,MISC2,MOUNT,NAME2,REMOVE,SKIP",
};
```

`set_output_verbosity()` (`options.c:513-524`) applies tier `0..verbose`
through `parse_output_words(..., DEFAULT_PRIORITY)`. Tier 0 fires
unconditionally, which is why `NONREG` is on at level 1 by default.

`-q` (quiet) negates everything via `negate_output_levels()`
(`options.c:569-578`), so `INFO_GTE(...)` returns false until a later
`--info=` raises a flag back.

### 1.3 `--stats` and `--progress` short-cut implications

- `--stats`: `options.c:2046-2048` lifts `STATS` to level 2 (or 3 when
  `verbose > 1`).
- `--progress`: `options.c:2342-2346` raises `PROGRESS` to 1 (or 2 for
  `progress2`/`info=progress2`) and, when `NAME` is `EQ(0)`, lifts `NAME`
  to 1; it also lifts `FLIST` to 2.

These lifts run at `DEFAULT_PRIORITY`, so an explicit `--info=stats0` or
`--info=noname` overrides them per the priority test at
`options.c:457-460`.

### 1.4 Token grammar

`parse_output_words()` (`options.c:427-471`) tokenises a comma-separated
list, lower-casing implicit, and accepts:

- A bare flag name (`name`) sets level 1.
- A trailing digit (`flist2`) sets that level, clamped to `MAX_OUT_LEVEL`.
- `none` clears every flag (`len = lev = 0`).
- `all` sets every flag to `lev` (default 1).
- `help` triggers `output_item_help()` and exits.
- Unknown tokens print `Unknown --info item: "<tok>"` and exit
  `RERR_SYNTAX` (1) - but only when `!am_server` (line 465).

The `no` / `-` negation prefix that the `--debug` man page documents is
**not** part of `parse_output_words` itself; upstream relies on `lev=0`
via the digit suffix (e.g. `noprogress` would be rejected).

## 2. oc-rsync flag table

### 2.1 Storage

`crates/logging/src/levels/info.rs:14-43` defines `InfoFlag` with the
thirteen upstream variants in the same order. `InfoLevels`
(`crates/logging/src/levels/info.rs:51-80`) holds a `u8` per flag, mirroring
`info_levels[]`.

### 2.2 Two parallel front-ends

oc-rsync ships two near-duplicate `--info` parsers:

1. `crates/cli/src/frontend/execution/flags/info.rs:62-191` -
   `InfoFlagSettings::apply`, used by the CLI driver. Capped levels live
   here per-arm.
2. `crates/cli/src/frontend/info_output.rs:237-340` - `parse_info_flags`
   returning a wrapped `InfoLevels`. Uncapped per-flag, only used by tests
   and `from_verbosity`.
3. `crates/logging/src/config.rs:203-225` - `apply_info_flag`, the
   thread-local logging config path. The CLI driver also funnels every
   token through this at
   `crates/cli/src/frontend/execution/drive/options.rs:145-154`.

Only the first two surface results to the run-time driver. `progress`
and `stats` are read directly off `InfoFlagSettings`
(`drive/options.rs:133-139`); everything else is propagated via
`logging::apply_info_flag` and then consulted by `info_log!` /
`info_gte` at point-of-emission (`crates/logging/src/macros.rs:31-37`,
`crates/logging/src/thread_local.rs:67-83`).

### 2.3 Default verbosity ladder

`crates/logging/src/config.rs:49-130` (`VerbosityConfig::from_verbose_level`)
hard-codes:

- Level 0: `nonreg=1` only.
- Level 1: adds `copy=del=flist=misc=name=stats=symsafe=1`.
- Level 2: adds `misc=2,name=2,backup=mount=remove=skip=1`.

Level 2 and 3 are identical for info flags (debug rises with level 3+).
This matches upstream's `info_verbosity[0..=2]` literally except that
upstream lacks any verbosity tier above 2 for info flags.

## 3. Per-flag verbosity matrix

For each flag the table records:

- **Max level upstream** - the largest `N` that any `INFO_GTE(<flag>, N)`
  call uses in upstream's source tree.
- **Help-text levels** - what `options.c:271-283` advertises.
- **oc-rsync cap** - the parse-time guard in
  `crates/cli/src/frontend/execution/flags/info.rs`. `-` means uncapped.
- **oc-rsync dispatch** - whether oc-rsync still emits something at that
  level (`info_log!(<Flag>, N, ...)` or driver-level handling).

| Flag | Max upstream lvl | Help-text levels | oc-rsync parse cap | oc-rsync max dispatched lvl | Status |
|------|------------------|------------------|--------------------|------------------------------|--------|
| BACKUP   | 1 (`main.c:1005-1007`, `generator.c:1977`)           | implicit 1 | uncapped | 1 (tests only)               | PARTIAL |
| COPY     | 1 (`generator.c:919`)                                | implicit 1 | uncapped | 1 (tests only)               | PARTIAL |
| DEL      | 1 (`log.c:864`)                                      | implicit 1 | uncapped | 1 (`engine/.../cleanup.rs:152,165,181,303,316`, `transfer/.../directory/deletion.rs:166,169,172`) | YES |
| FLIST    | 2 (`flist.c:176,183`, `generator.c:385`)             | 1-2        | 2 (`info.rs:124-130`) | 1 (`flist/parallel.rs:44`, `transfer/.../generator/file_list/mod.rs:56,102,131,218`, `.../filters.rs:157`, `.../receiver/transfer.rs:912`) | PARTIAL |
| MISC     | 2 (`io.c:1536`)                                      | 1-2        | 2 (`info.rs:131-137`) | 1 (`receiver/file_list.rs:354,381,395`, `receiver/directory/creation.rs:83,267,284,300,312,326`) | PARTIAL |
| MOUNT    | 1 (`generator.c:325`)                                | implicit 1 | uncapped | 0 (no `info_log!(Mount, ...)`) | MISSING |
| NAME     | 2 (`log.c:826`, `generator.c:575,998,1009,1030,1127,1133`, `hlink.c:223,401,404`) | 1-2 | 2 (`info.rs:99-110`) | 1 (`transfer/receiver/transfer.rs:148,150,174,639,641`, `.../transfer/pipeline.rs:230,257,437`, `.../directory/creation.rs:338,340`, `.../directory/links.rs:62`) | PARTIAL |
| NONREG   | 1 (`generator.c:1684`)                               | default 1, 0 disables | uncapped | 0 (no production `info_log!(Nonreg, ...)`) | MISSING |
| PROGRESS | 2 (`progress.c:83,172,200`, `receiver.c:565`)        | 1-2        | 2 (`info.rs:83-91`) | 1 + 2 via `ProgressSetting::Overall` (`drive/options.rs:133-136`, `crates/cli/src/frontend/progress/diagnostic.rs:90,131,157,167`) | YES |
| REMOVE   | 1 (`sender.c:175`)                                   | implicit 1 | uncapped | 1 (`engine/.../cleanup.rs:369`) | YES |
| SKIP     | 2 (`generator.c:1367,1385,1387,1693,1701,1710`)      | 1-2        | 2 (`info.rs:150-156`) | 1 (`engine/.../file/copy/mod.rs:75,82`, `receiver/transfer/candidates.rs:80`, `receiver/directory/creation.rs:267`) | PARTIAL |
| STATS    | 3 (`main.c:333`, `generator.c:2377,2422`)            | 1-3        | 3 (`info.rs:92-98`)  | 1 surfaced as `stats: bool` in `drive/options.rs:137-139` | PARTIAL |
| SYMSAFE  | 1 (`backup.c:291`, `flist.c:216`)                    | implicit 1 | uncapped | 0 (no production `info_log!(Symsafe, ...)`) | MISSING |

### 3.1 Notes on the table

- **Status legend**: `YES` = every upstream-emitted level dispatches a
  matching oc-rsync emission. `PARTIAL` = level 1 dispatches but
  upstream's higher levels (2 or 3) emit lines that oc-rsync currently
  swallows. `MISSING` = no oc-rsync production code emits the flag at
  any level.
- The parse cap of `info.rs` clamps user input only; lifting it does
  nothing on its own because the dispatch sites do not differentiate
  level 2 from level 1. See section 4.
- **NAME level 2** is the single most pervasive divergence: ten upstream
  call sites at `INFO_GTE(NAME, 2)` change the output that
  `--itemize-changes` interacts with (`log.c:826`, `generator.c:575`,
  `generator.c:998-1133`, `hlink.c:223,401,404`). oc-rsync's
  `NameOutputLevel::UpdatedAndUnchanged` is parsed
  (`info.rs:99-110`) and stored, but no production `info_log!` or
  `should_show_*` consumer reads back the `UpdatedAndUnchanged` case
  outside the progress observer.
- **STATS 2 and 3** map to the audit in #2111: line 333 (`main.c`) gates
  the `show_malloc_stats()` / `show_flist_stats()` extras, and line 418
  gates the per-line block. oc-rsync collapses STATS to a single
  boolean (`drive/options.rs:138`), which loses the level-2-vs-3 split
  upstream uses to surface flist/malloc stats.
- **NONREG default 1**: upstream's `info_verbosity[0]` puts NONREG on
  even before `-v`. `VerbosityConfig::from_verbose_level(0)` matches
  this (`config.rs:48-49`), but the actual emission site
  (`generator.c:1684`) has no production analogue in oc-rsync, so the
  warning text is never printed.
- **SYMSAFE / MOUNT**: both default to off, both flagged on at `-v` /
  `-vv` per `info_verbosity`. oc-rsync sets the levels in
  `VerbosityConfig` but no code reads them back, so the flag is a no-op.
- **`--info=progress2`**: handled correctly. The parse table in
  `info.rs:83-91` maps level 2 to `ProgressSetting::Overall`, which
  reaches `ClientProgressObserver` via `info_result.progress_setting`
  (`drive/options.rs:563`). See `docs/audits/progress-line-format.md`
  for the line-format consequences.

## 4. Cross-cutting gaps

The parser accepts every documented flag and digit but the dispatch
side only inspects "level > 0" for most flags. Concrete consequences:

### 4.1 Levels above 1 collapse silently

`crates/cli/src/frontend/info_output.rs:106-196` defines twelve
`should_show_*` helpers; every one returns `levels.get(flag) > 0`. The
`info_log!` macro tests `info_gte(flag, $level)` with the level the call
site specifies, so an emission at `info_log!(Foo, 1, ...)` will fire
whether the user passed `--info=foo`, `--info=foo2`, or `--info=foo3`.
There is no second emission for the higher level, because oc-rsync has
no `info_log!(Foo, 2, ...)` (or `3`) in production paths.

### 4.2 `none` / `all` semantics

`apply()` in `info.rs:70-78` honours both, but with one wrinkle: the
"all" arm sets `progress = PerFile` (level 1) and `stats = Some(1)`. It
matches upstream `parse_output_words("all", lev=1)` in spirit. Upstream
accepts `all<N>` (e.g. `all2`) to set every flag to level N (per-flag
clamped via the `lev > MAX_OUT_LEVEL` ceiling in
`options.c parse_output_words`). oc-rsync additionally accepts a bare
`<N>` token like `--info=2` with the same semantics as a usability
extension; the dispatch flows through `enable_all_at_level(N)` so the
per-flag caps stay in lockstep with the per-token validation in
`apply()`.

### 4.3 `no<flag>` / `-<flag>` negation

`crates/cli/src/frontend/execution/flags/info.rs::parse_flag_and_level`
accepts the `no` and `-` prefixes by stripping them and treating the
level as `0`. Upstream lacks this codepath in `parse_output_words`; it
relies on `stats0` etc. The diversion is invisible because both
ultimately set the level to zero, but the rsync man page
(`rsync.1.md:419`) uses the suffix form only. The forms are retained for
backwards compatibility and tolerance of server-mode token forwarding,
but are no longer advertised in `--info=help`; the suffix form is the
only spelling shown to users.

### 4.4 Unknown-token error code

oc-rsync surfaces unknown info tokens with `rsync_error!(1, ...)` at
`info.rs:194-200`. Upstream uses `RERR_SYNTAX` (1) at
`options.c:466-468`. Same numeric code, but the message string differs:

- Upstream: `Unknown --info item: "FOO"`.
- oc-rsync: `invalid --info flag 'FOO': use --info=help for supported flags`.

### 4.5 Server-side tolerance

Upstream skips the unknown-token error when `am_server` is true
(`options.c:465`). oc-rsync errors uniformly. For a server invoked over
SSH this can surface as a stricter parse than upstream, which can break
forward-compatibility with newer client flag spellings.

### 4.6 `priority` field not modelled

Upstream tracks per-flag priority (`DEFAULT_PRIORITY < HELP_PRIORITY <
USER_PRIORITY < LIMIT_PRIORITY`, `options.c:249-252`) so a later
`-v` cannot lower a level that the user pinned with `--info=name2`.
oc-rsync overwrites unconditionally on every `set()` call
(`crates/logging/src/levels/info.rs:104-120`); the documented order
in `crates/cli/src/frontend/execution/drive/options.rs` happens to call
`parse_info_flags` after `VerbosityConfig::from_verbose_level`, so
`--info=` settings win in practice. But if `limit_output_verbosity()`
(`options.c:527-552`) ever needs to be matched (server-side ceiling
imposed by daemon config), the missing priority field will block it.

## 5. Summary of divergences

Status legend: FIXED in this audit; OPEN remains a divergence; CLOSED was
re-evaluated and the implementation already matches upstream.

| ID  | Flag/area | Severity | Status | Description |
|-----|-----------|----------|--------|-------------|
| I1  | NAME 2    | High     | OPEN   | Ten upstream call sites at `INFO_GTE(NAME, 2)` (`log.c:826`, `generator.c:575,998,1009,1030,1127,1133`, `hlink.c:223,401,404`) have no oc-rsync counterpart; unchanged-name and itemize-changes-amplifying output is silently dropped. |
| I2  | STATS 2/3 | High     | OPEN   | `crates/cli/src/frontend/execution/drive/options.rs:137-139` reduces stats to `bool`; upstream's level 2 (`main.c:418`) vs 3 (`main.c:333`) split is lost. Covered by audit #2111. |
| I3  | NONREG    | Medium   | OPEN   | Upstream emits at `generator.c:1684` whenever `NONREG >= 1` (on by default); no oc-rsync `info_log!(Nonreg, ...)` exists, so the warning is never printed. |
| I4  | MOUNT     | Medium   | OPEN   | Upstream emits at `generator.c:325-327` for mount-point delete skips; no oc-rsync emission. |
| I5  | SYMSAFE   | Medium   | OPEN   | Upstream emits at `flist.c:216` and `backup.c:291`; no oc-rsync emission. |
| I6  | BACKUP 1  | Medium   | OPEN   | Upstream emits at `main.c:1005-1007` and `generator.c:1977-1980`; no oc-rsync `info_log!(Backup, ...)` in production source. |
| I7  | COPY 1    | Medium   | OPEN   | Upstream emits at `generator.c:919-922` when local copy fails; no oc-rsync `info_log!(Copy, ...)` in production source. |
| I8  | FLIST 2   | Low      | OPEN   | Upstream emits "N files to consider" at `flist.c:183-185` and the running counter at `flist.c:176`; oc-rsync only emits at level 1 ("built file list with N entries"). |
| I9  | MISC 2    | Low      | OPEN   | Upstream emits "Setting --timeout=N to match server" at `io.c:1536-1537`; oc-rsync only emits MISC-1 errors. |
| I10 | SKIP 2    | Low      | OPEN   | Upstream emits the "(type change)" / "(sum change)" suffix at `generator.c:1387`; oc-rsync's SKIP emissions never include the suffix. |
| I11 | All       | Medium   | OPEN   | `should_show_*` and the parse-cap layer collapse level 2/3 to level 1 because no production code differentiates levels beyond `> 0`. Closing I1-I10 individually addresses this category. |
| I12 | All       | Low      | RESOLVED | `--info=N` (bare digit) now accepted as a usability extension; oc-rsync's `apply()` in `info.rs` delegates to `enable_all_at_level(N)` with the same per-flag caps as upstream's `all<N>` token (`options.c parse_output_words`). |
| I13 | All       | Low      | RESOLVED | `--info=no<flag>` / `--info=-<flag>` are an internal-only extension not advertised in `--info=help`; the parser still accepts them for backwards compatibility and server-mode token forwarding, but the user-facing surface only shows the upstream suffix form. |
| I14 | All       | Low      | OPEN   | Unknown-token message text differs: upstream `Unknown --info item: "FOO"`, oc-rsync `invalid --info flag 'FOO': use --info=help for supported flags`. Same exit code (1). |
| I15 | All       | Low      | OPEN   | Server-side mode (`am_server`) should suppress unknown-token errors (`options.c:465`); oc-rsync errors unconditionally. |
| I16 | All       | Low      | OPEN   | Priority field (`DEFAULT/HELP/USER/LIMIT`, `options.c:249-252`) not modelled; later daemon-imposed limits cannot lower user-set levels. |
| I17 | Help text | Low      | OPEN   | Help text in `info.rs:228-246` advertises hard-coded `(levels 1-2)` / `(levels 1-3)` ranges; upstream's `output_item_help()` (`options.c:474-509`) prints the per-verbosity additions dynamically. The two diverge whenever upstream's `info_verbosity[]` is edited. |

## 6. Recommended fixes

### P0 - Behavioral parity

1. **I1 - NAME 2 emissions**: thread the level distinction through
   `crates/transfer/src/receiver/transfer.rs` and
   `crates/transfer/src/receiver/transfer/pipeline.rs`. Each existing
   `info_log!(Name, 1, ...)` site needs a sibling `info_log!(Name, 2,
   ...)` for unchanged names where upstream emits one (see
   `generator.c:998-1133` for the full set).
2. **I2 - STATS levels**: replace the `stats: bool` field with a `u8`
   level on `InfoFlagsResult` and route STATS 3 through the
   `show_malloc_stats` / `show_flist_stats` emitters. Follow-up of
   audit #2111.

### P1 - Missing emissions for default-on flags

3. **I3 - NONREG**: add `info_log!(Nonreg, 1, "skipping non-regular
   file \"{}\"", fname)` at the receiver's regular-vs-special branch
   (mirror `generator.c:1684-1688`).
4. **I5 - SYMSAFE**: emit at the symlink-safety reject sites
   (`crates/transfer/src/receiver/directory/links.rs:62`) using
   `info_log!(Symsafe, 1, ...)` instead of `Name, 1`.
5. **I4 - MOUNT**: emit when delete encounters a mount-point boundary
   (`generator.c:325-327`).

### P2 - Round-trip parity for explicit flags

6. **I6 / I7 - BACKUP, COPY**: backfill emissions where oc-rsync
   currently logs through other channels (e.g. backup messages currently
   go through `Message` rather than `info_log!`).
7. **I8 / I9 / I10 - FLIST 2, MISC 2, SKIP 2**: add the missing
   `info_log!(<Flag>, 2, ...)` sites. These are visible only when the
   user explicitly asks for level 2, so they are low-impact but useful
   for grep-based downstream pipelines.

### P3 - Parser polish

8. **I12 - bare digit token** (RESOLVED): `apply()` now accepts any pure
   digit token (`"2"`, `"3"`, ...) and routes it through
   `enable_all_at_level(N)` to mirror upstream `all<N>` per-flag caps.
9. **I14 / I15 - unknown-token text and server tolerance**: align the
   string to `Unknown --info item: "FOO"` and short-circuit when
   running as server.
10. **I16 - priority field**: add a `priority: u8` companion to each
    `InfoLevels` field and honour the order in
    `crates/logging/src/config.rs`. Most flows will continue to use
    `USER_PRIORITY`; daemon mode is the consumer.

## 7. Test plan

Add fixtures under `crates/cli/tests/fixtures/info_flags/` keyed by
`(flag, level, sender_or_receiver)`:

- `flist1.golden` / `flist2.golden` - capture upstream's "building file
  list..." vs "N files to consider" outputs.
- `name1.golden` / `name2.golden` - per-file lines vs the
  itemize-changes amplifier for unchanged paths.
- `skip1.golden` / `skip2.golden` - basic SKIP message vs the
  `(type change)` / `(sum change)` suffix.
- `stats1.golden` / `stats2.golden` / `stats3.golden` - traffic-only
  trailer vs full block vs block-plus-malloc-and-flist stats.
- `nonreg_default.golden` - confirms the default-on `NONREG` warning
  fires without `-v`.
- `mount.golden`, `symsafe.golden`, `backup.golden`, `copy.golden`,
  `remove.golden` - direct fixtures for each missing emission.

Each fixture should be captured with `LC_ALL=C rsync ... 2>&1 | hexdump
-C` against the local 3.4.1 build and stored verbatim. Tests assert
`assert_eq!` against oc-rsync output (not `contains`) to catch
whitespace and ordering drift.

Add property tests for the parser:

- Every flag name in `info_words[]` is accepted by oc-rsync at every
  level `0..=4`.
- `--info=help` produces the same help body as upstream (line-by-line
  diff against `output_item_help` output captured from upstream).
- `--info=2` (bare digit) parity (covers I12).
- `--info=foo,bar` where `foo` is unknown emits exit 1 with the
  upstream-format message (covers I14) - except in `am_server` mode
  (covers I15).

## 8. Upstream source references

- `rsync.h:1416` - `INFO_GTE(flag, lvl)` macro.
- `rsync.h:1421-1435` - `INFO_BACKUP..INFO_SYMSAFE`, `COUNT_INFO`.
- `options.c:228-243` - `debug_verbosity[]` and `info_verbosity[]`
  tiers.
- `options.c:245` - `MAX_OUT_LEVEL 4`.
- `options.c:247` - `info_levels[]` global storage.
- `options.c:249-257` - priority and `W_*` direction bits.
- `options.c:268-285` - `INFO_WORD` macro and `info_words[]` table.
- `options.c:344-425` - `make_output_option()` round-trips the levels
  back to a `--info=` string.
- `options.c:427-471` - `parse_output_words()` - tokeniser.
- `options.c:473-510` - `output_item_help()` - the `--info=help` body.
- `options.c:513-524` - `set_output_verbosity()`.
- `options.c:527-552` - `limit_output_verbosity()` (server ceiling).
- `options.c:555-577` - `reset_output_levels()` / `negate_output_levels()`.
- `options.c:1754-1757` - `OPT_INFO` switch arm.
- `options.c:2046-2048` - `--stats` -> STATS 2/3 lift.
- `options.c:2342-2346` - `--progress` -> PROGRESS 1/2 + FLIST 2 + NAME 1.

Emission sites consulted in this audit:

- `backup.c:291` - SYMSAFE 1.
- `flist.c:152,176,183,216` - FLIST 1/2, SYMSAFE 1.
- `flist.c:2216,2571` - FLIST 1.
- `generator.c:325,385` - MOUNT 1, FLIST 2.
- `generator.c:575,998,1009,1030,1127,1133` - NAME 2.
- `generator.c:919` - COPY 1.
- `generator.c:1367,1385,1387,1693,1701,1710` - SKIP 1/2.
- `generator.c:1492,1548,1599,1671` - NAME 1.
- `generator.c:1684` - NONREG 1.
- `generator.c:1977` - BACKUP 1.
- `generator.c:2377,2422` - STATS 2.
- `hlink.c:223,236,401,404,460` - NAME 1/2.
- `io.c:1536` - MISC 2.
- `log.c:826` - NAME 2.
- `log.c:864` - DEL 1.
- `main.c:333` - STATS 3.
- `main.c:418,451` - STATS 2/1.
- `main.c:732,798` - NAME 1.
- `main.c:1005-1007` - BACKUP 1.
- `main.c:1616` - PROGRESS 1.
- `progress.c:83,139,161,172,200` - PROGRESS 1/2, NAME 1.
- `receiver.c:294,302,316,395,565,888,949` - PROGRESS 1/2, NAME 1.
- `sender.c:175` - REMOVE 1.
- `util1.c:512` - MISC 1.
- `cleanup.c:224` - STATS 1.
- `batch.c:143` - MISC 1.
- `match.c:135,376,384` - PROGRESS 1.

## 9. oc-rsync source references

- `crates/logging/src/levels/info.rs:14-137` - `InfoFlag`, `InfoLevels`
  storage and accessors.
- `crates/logging/src/config.rs:42-194` - `VerbosityConfig::from_verbose_level`
  verbosity ladder.
- `crates/logging/src/config.rs:203-225` - `apply_info_flag` tokeniser.
- `crates/logging/src/macros.rs:31-37` - `info_log!` macro.
- `crates/logging/src/thread_local.rs:67-83` - `info_gte` / `emit_info`.
- `crates/cli/src/frontend/execution/flags/info.rs:11-201` -
  `InfoFlagSettings`, `apply`, help-text body.
- `crates/cli/src/frontend/execution/drive/options.rs:95-165` -
  `parse_info_settings` dispatch entry point and shim that re-routes
  every token through `logging::apply_info_flag` (`drive/options.rs:145-154`).
- `crates/cli/src/frontend/info_output.rs:50-340` - public
  `InfoFlags` wrapper, `parse_info_flags`, `should_show_*` helpers.
- `crates/cli/src/frontend/progress/diagnostic.rs:90-188` -
  `InfoFlag::Progress` / `InfoFlag::Stats` emission for progress
  observer diagnostics.

Production `info_log!` call sites (the complete set as of
`origin/master`):

- `crates/flist/src/parallel.rs:44` - `Flist, 1`.
- `crates/transfer/src/generator/file_list/mod.rs:56,102,131,218` - `Flist, 1`.
- `crates/transfer/src/generator/filters.rs:157` - `Flist, 1`.
- `crates/transfer/src/receiver/file_list.rs:354,381,395` - `Misc, 1`.
- `crates/transfer/src/receiver/transfer.rs:148,150,174,639,641,912` -
  `Name, 1` and `Flist, 1`.
- `crates/transfer/src/receiver/transfer/candidates.rs:80` - `Skip, 1`.
- `crates/transfer/src/receiver/transfer/pipeline.rs:230,257,437` - `Name, 1`.
- `crates/transfer/src/receiver/directory/creation.rs:83,267,284,300,312,326,338,340` -
  `Misc, 1`, `Skip, 1`, `Name, 1`.
- `crates/transfer/src/receiver/directory/deletion.rs:166,169,172` - `Del, 1`.
- `crates/transfer/src/receiver/directory/links.rs:62` - `Name, 1` (see I5).
- `crates/engine/src/local_copy/executor/cleanup.rs:152,165,181,303,316,369` -
  `Del, 1` and `Remove, 1`.
- `crates/engine/src/local_copy/executor/file/copy/mod.rs:75,82` - `Skip, 1`.

Note the absence of any `info_log!(Backup, ...)`,
`info_log!(Copy, ...)`, `info_log!(Mount, ...)`, `info_log!(Nonreg, ...)`,
or `info_log!(Symsafe, ...)` outside the test tree, which directly
maps to divergences I3-I7. Note also the absence of any production
call at level 2 or 3, which maps to I11.
