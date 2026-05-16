# --info=FLAGS verbosity audit

Reference audit comparing upstream rsync 3.4.1 `--info=FLAGS` semantics against
oc-rsync's parsing and gating. Upstream is the only source of truth.

Sources reviewed:

- Upstream tables and parser: `target/interop/upstream-src/rsync-3.4.1/options.c`
  lines 228-577 (`info_verbosity[]`, `info_words[]`, `parse_output_words`,
  `set_output_verbosity`, `limit_output_verbosity`, `make_output_option`,
  `output_item_help`).
- Upstream constants: `target/interop/upstream-src/rsync-3.4.1/rsync.h`
  lines 1416-1435 (`INFO_GTE`, `INFO_EQ`, `INFO_BACKUP..INFO_SYMSAFE`,
  `COUNT_INFO`).
- Upstream gating call sites: `backup.c`, `batch.c`, `cleanup.c`, `flist.c`,
  `generator.c`, `hlink.c`, `io.c`, `log.c`, `main.c`, `match.c`, `progress.c`,
  `options.c`.
- Upstream daemon table: `target/interop/upstream-src/rsync-3.4.1/loadparm.c`
  has no `info_levels` references; daemon module config does not expose info
  flags directly.
- Our parser: `crates/cli/src/frontend/execution/flags/info.rs`,
  `crates/cli/src/frontend/info_output.rs`,
  `crates/cli/src/frontend/execution/drive/options.rs`,
  `crates/logging/src/levels/info.rs`, `crates/logging/src/config.rs`,
  `crates/logging/src/thread_local.rs`, `crates/logging/src/macros.rs`.

## Master matrix

Upstream max level is the highest `OPTN` documented for that flag in
`info_words[]` (defaulting to `MAX_OUT_LEVEL = 4` when the help string does not
constrain it). Where parsing differs from upstream, the cell is flagged. Gating
sites list every production code path that consults the flag; tests are
excluded.

| Flag      | Upstream max level | Description (upstream)                                             | Our parsing                                                                                                                                                                                                                                                                                                                          | Our gating sites                                                                                                                                                                          | Parity      |
| --------- | ------------------ | ------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------- |
| BACKUP    | 1 (W_REC)          | Mention files backed up                                            | `crates/logging/src/levels/info.rs:54-138` (`InfoLevels::backup`); `crates/cli/src/frontend/execution/flags/info.rs:112-115` accepts arbitrary level (no upper cap, see `parse_flag_and_level`); `crates/cli/src/frontend/info_output.rs:325` parses identically.                                                                       | `crates/engine/src/local_copy/context_impl/state.rs` emits `info_log!(Backup, 1, "backed up {} to {}", ...)` at the end of `backup_existing_entry`, mirroring `backup.c:352` (`INFO_GTE(BACKUP, 1)` on the success label). Covers rename, replace-then-rename, and cross-device fallback paths.                                          | Match       |
| COPY      | 1 (W_REC)          | Mention files copied locally on the receiving side                 | Same path as BACKUP; `crates/cli/src/frontend/execution/flags/info.rs:116-119` accepts any level (no cap).                                                                                                                                                                                                                            | `crates/transfer/src/receiver/quick_check.rs::try_reference_dest` emits `copy_file <src> => <dst>: <err> (<errno>)` on `--copy-dest` `fs::copy` failure via `info_log!(Copy, 1, ...)`. `crates/engine/src/local_copy/executor/file/copy/transfer/execute.rs::execute_transfer` emits the same wording when opening the alt-base override fails. Both mirror upstream `generator.c:919` `INFO_GTE(COPY, 1)` inside `copy_altdest_file()`. | Match       |
| DEL       | 1 (W_REC)          | Mention deletions on the receiving side                            | `crates/cli/src/frontend/execution/flags/info.rs:120-123`, `info_output.rs:327`. No upper cap.                                                                                                                                                                                                                                       | None in production. `should_show_del` exists but no transfer/engine site gates on it.                                                                                                                                                                                                                                                       | Stub        |
| FLIST     | 2 (W_CLI)          | Mention file-list receiving/sending (levels 1-2)                   | `crates/cli/src/frontend/execution/flags/info.rs:124-130` rejects `level > 2`; `info_output.rs:328` does not enforce the cap.                                                                                                                                                                                                         | None in production. Upstream uses `INFO_GTE(FLIST, 1/2)` in `flist.c` and `generator.c`; our flist code in `crates/transfer/src/file_list/*` and `crates/engine/src/local_copy/*` does not call `info_gte(InfoFlag::Flist, _)`.                                                                                                          | Parsed-only |
| MISC      | 2 (W_SND\|W_REC)   | Mention miscellaneous information (levels 1-2)                     | `crates/cli/src/frontend/execution/flags/info.rs:131-137` rejects `level > 2`; `info_output.rs:329` does not enforce.                                                                                                                                                                                                                 | None in production. Upstream calls `INFO_GTE(MISC, 1/2)` in `batch.c:143` and `io.c:1536`. Equivalent paths in `crates/batch/src/*` and `crates/protocol/src/multiplex/*` lack the gate.                                                                                                                                                  | Parsed-only |
| MOUNT     | 1 (W_SND\|W_REC)   | Mention mounts that were found or skipped                          | `crates/cli/src/frontend/execution/flags/info.rs:138-141`, `info_output.rs:330`. No upper cap.                                                                                                                                                                                                                                       | `crates/engine/src/local_copy/executor/directory/recursive/entry.rs` and `crates/engine/src/local_copy/executor/sources/orchestration.rs` emit `info_log!(Mount, 1, "skipping mount-point dir {}", ...)` at the cross-device skip sites, mirroring `flist.c:1319` (`INFO_GTE(MOUNT, 1)` inside `send_file_list`). Both sites cover the recursive child-directory case and the root-level `-xx` source case. The receiver-side `cannot delete mount point` notice (`generator.c:325`) is not yet mirrored because `FLAG_MOUNT_DIR` is not propagated through our flist.                                                                                                                                                       | Match       |
| NAME      | 2 (W_SND\|W_REC)   | Mention 1) updated file/dir names, 2) unchanged names              | Upstream parses `name`/`name1`/`name2` into a numeric level. We map to `NameOutputLevel::{Disabled,UpdatedOnly,UpdatedAndUnchanged}` at `crates/cli/src/frontend/execution/flags/info.rs:99-110`; level >= 2 collapses to `UpdatedAndUnchanged`. Logging path keeps a numeric `name` field at `crates/logging/src/levels/info.rs:67`. | Mapped via `NameOutputLevel` and consumed by progress/itemize logic in `crates/cli/src/frontend/progress/*`. The implicit `INFO_EQ(NAME, 0)` and `INFO_GTE(NAME, 1)` upstream sites in `options.c:2342-2353` and `main.c:732,798` are mirrored by `name_overridden` and `log_before_transfer` plumbing in `drive/options.rs`.        | Partial     |
| NONREG    | 1 (W_REC, default 1; 0 disables) | Mention skipped non-regular files (default 1, 0 disables)           | `crates/cli/src/frontend/execution/flags/info.rs:142-145`, `info_output.rs:332`. Upstream's "default 1" semantics live in `info_verbosity[0]`; we set `nonreg = 1` for all `-v` levels including 0 at `crates/logging/src/config.rs:49,53,65,..`.                                                                                          | `crates/engine/src/local_copy/context_impl/reporting.rs:11` emits `info_log!(Nonreg, 1, "skipping non-regular file \"%s\"", ...)` from `record_skipped_non_regular`, mirroring `generator.c:1687`. The single funnel covers symlink/fifo/device source skips routed through the local-copy executor.                                              | Match       |
| PROGRESS  | 2 (W_CLI)          | Mention 1) per-file progress or 2) total transfer progress         | `crates/cli/src/frontend/execution/flags/info.rs:83-91`: level 0 -> `Disabled`, 1 -> `PerFile`, 2 -> `Overall`, 3+ -> error. `info_output.rs:333` parses identically; `info_output.rs:441` shows `progress2` setting level 2 directly.                                                                                                | `crates/cli/src/frontend/progress/diagnostic.rs:90,131,157,188` emits via `emit_info(InfoFlag::Progress, _, _)` and `crates/cli/src/frontend/progress/live.rs:117` switches between modes. Upstream's `INFO_GTE(PROGRESS, 1/2)` sites in `match.c:135/376/384`, `progress.c:83/139`, `flist.c:3411`, `main.c:1616` are covered.       | Match       |
| REMOVE    | 1 (W_SND)          | Mention files removed on the sending side                          | `crates/cli/src/frontend/execution/flags/info.rs:146-149`, `info_output.rs:334`. No upper cap.                                                                                                                                                                                                                                       | None in production. Upstream `--remove-source-files` notifications gate on `INFO_GTE(REMOVE, 1)`; we have `--remove-source-files` plumbing in `crates/core` but no info gate before logging.                                                                                                                                            | Stub        |
| SKIP      | 2 (W_REC)          | Mention files skipped due to transfer overrides (levels 1-2)       | `crates/cli/src/frontend/execution/flags/info.rs:150-156` rejects `level > 2`; `info_output.rs:335` does not enforce.                                                                                                                                                                                                                | None in production. Upstream `INFO_GTE(SKIP, 1/2)` fires in `generator.c:1367/1385/1387/1693/1701/1710`. Equivalent generator paths in `crates/engine/src/generator/*` do not consult the gate.                                                                                                                                          | Parsed-only |
| STATS     | 3 (W_CLI\|W_SRV)   | Mention statistics at end of run (levels 1-3)                      | `crates/cli/src/frontend/execution/flags/info.rs:92-98` rejects `level > 3`; `info_output.rs:336` does not enforce.                                                                                                                                                                                                                  | `crates/cli/src/frontend/progress/diagnostic.rs:167` and `crates/cli/src/frontend/execution/drive/summary.rs` consume the boolean derived from `stats > 0`. Upstream's three-level emission (`STATS, 1/2/3` in `cleanup.c:224`, `generator.c:2377/2422`, `main.c:333/418/451`) is collapsed into a single boolean in our renderer. | Partial     |
| SYMSAFE   | 1 (W_SND\|W_REC)   | Mention symlinks that are unsafe                                   | `crates/cli/src/frontend/execution/flags/info.rs:157-160`, `info_output.rs:337`. No upper cap.                                                                                                                                                                                                                                       | `crates/transfer/src/generator/file_list/walk.rs::resolve_symlink_metadata` and the batched-stat fixup in the same file emit `copying unsafe symlink "<path>" -> "<target>"` via `info_log!(Symsafe, 1, ...)` on the sender side when `--copy-unsafe-links` triggers a dereference. `crates/engine/src/local_copy/executor/special/symlink.rs::copy_symlink` emits the same notice on the local-copy path. `crates/engine/src/local_copy/context_impl/state.rs::backup_existing_entry` emits `not backing up unsafe symlink "<dest>" -> "<target>"` on the cross-device backup fallback when `--safe-links` refuses to recreate the symlink. All three mirror upstream `flist.c:217` and `backup.c:292`. | Match       |

Legend:

- **Match**: parsing and at least one production gating site equivalent to
  upstream.
- **Partial**: parsing maps to a different shape (boolean or enum) but the
  resulting setting is consumed in production; some upstream call sites are not
  mirrored.
- **Parsed-only**: token accepted and stored, level cap correct, but no
  production code consults the level before emitting messages.
- **Stub**: token accepted and stored but neither the level cap nor the
  emission gating is implemented.

## --info=help

Upstream prints the table built by `output_item_help` (`options.c:474-510`),
which iterates `info_words[]`, then `ALL`, `NONE`, `HELP`, then
`Options added at each level of verbosity` for each verbosity row in
`info_verbosity[]`.

Our help text is a fixed string at
`crates/cli/src/frontend/execution/flags/info.rs:228-246` (`INFO_HELP_TEXT`).
The drive layer prints it at
`crates/cli/src/frontend/execution/drive/options.rs:123-131` and exits with
status 0 on `help_requested`. Differences from upstream:

- We do not print the per-verbosity-level summary block (`0) NONREG`,
  `1) COPY,DEL,...`, `2) BACKUP,MISC2,...`).
- We document `no` and `-` prefix forms (`--info=noprogress`) and level
  suffixes (`--info=stats2`, `--info=flist0`); upstream documents the prefix
  form only via the parser's `none`/`all` keywords.
- We omit upstream's first line `Use OPT or OPT1 for level 1 output, OPT2 for
  level 2, etc.; OPT0 silences.`

## --info=NONE

Upstream `parse_output_words` (`options.c:450-451`) treats `NONE` as
`len = lev = 0`, then enters the loop with `len == 0` so it falls through to
the `if (!len ...)` branch (`options.c:454-463`) and assigns level 0 to every
flag.

Our parser handles `NONE` in two places:

- `crates/cli/src/frontend/execution/flags/info.rs:75-78` calls
  `disable_all()`, which sets every parsed setting to its disabled variant
  including `progress = Disabled`, `stats = Some(0)`, and
  `name = Some(NameOutputLevel::Disabled)`.
- `crates/cli/src/frontend/info_output.rs:283-286` calls
  `levels.set_all(0)` on the underlying `InfoLevels`.

These two parsers run in parallel: `drive/options.rs:145-153` invokes
`logging::apply_info_flag` per token after the high-level parser has already
applied its mapping. Because `none` falls through `parse_flag_and_level` (the
high-level parser handles it first and returns) and `apply_info_flag`
delegates to `info_output::parse_info_flags`, both execute. Behaviour matches
upstream as long as the keyword is not mixed with subsequent tokens that
re-enable a flag.

## progress2 alias

Upstream parses `progress2` via `parse_output_words` matching `progress`
against the `info_words` entry and reading the trailing `2` digit
(`options.c:439-445`). The numeric level is stored in `info_levels[INFO_PROGRESS]`,
and `INFO_GTE(PROGRESS, 2)` in `progress.c:83` switches the renderer to overall
mode.

Our parser also accepts the trailing-digit form and maps it to
`ProgressSetting::Overall` at
`crates/cli/src/frontend/execution/flags/info.rs:84-89`. The downstream
renderer at `crates/cli/src/frontend/progress/mode.rs:42` selects
`ProgressMode::Overall`. The hidden mapping in
`crates/cli/src/frontend/info_output.rs:441` sets the underlying level field
to 2 directly, so any consumer that reads `levels.get(InfoFlag::Progress)` sees
the numeric value upstream uses.

Upstream also auto-applies `parse_output_words(info_words, info_levels,
"FLIST2,PROGRESS", DEFAULT_PRIORITY)` whenever `do_progress && !am_server`
(`options.c:2345`). Our parser does not perform this implication: setting
`--progress` does not raise `flist` to level 2 anywhere in
`crates/cli/src/frontend/arguments/parser/mod.rs:546-553` or
`drive/options.rs`. This is a parity gap.

Upstream further enforces `parse_output_words(info_words, info_levels, "name",
DEFAULT_PRIORITY)` when `do_progress && !log_before_transfer && INFO_EQ(NAME,
0)` (`options.c:2343-2344`). Our equivalent `name` defaulting flows through
`InfoFlagSettings::name` and the `name_overridden` flag in
`drive/options.rs:140-143`, but `--progress` alone does not seed
`name = UpdatedOnly` the way upstream does.

## -P short option

Upstream `case 'P'` at `options.c:1600-1607` sets `do_progress = 1` and
`keep_partial = 1`; the auto-implication of `FLIST2,PROGRESS` then runs at
`options.c:2342-2346`.

We expose `-P` via clap's `partial-progress` argument
(`crates/cli/src/frontend/command_builder/sections/transfer_behavior_options.rs:250-252`).
The parser at
`crates/cli/src/frontend/arguments/parser/mod.rs:437,547` raises both
`partial` and `progress_setting = ProgressSetting::PerFile`. As with
`--progress`, the auto-implications are not propagated to `flist` or `name`.

## -v interaction with default levels

Upstream `set_output_verbosity` (`options.c:513-524`) iterates
`j = 0..=verbose` over `info_verbosity[]` and `debug_verbosity[]` with
`DEFAULT_PRIORITY`. The cumulative table at `options.c:239-243` is:

- 0: `NONREG`
- 1: `COPY,DEL,FLIST,MISC,NAME,STATS,SYMSAFE`
- 2: `BACKUP,MISC2,MOUNT,NAME2,REMOVE,SKIP`
- 3-5: no new info flags (debug only).

Subsequent `--info=FLAG` arguments use `USER_PRIORITY` and override the
defaults; `parse_output_words` only assigns when
`priority >= words[j].priority` (`options.c:457-460`). The result is that
explicit `--info=NAME0` after `-v` clears the implied level.

Our equivalent is `logging::VerbosityConfig::from_verbose_level`
(`crates/logging/src/config.rs:43-195`). The mapping matches upstream for
levels 0-5. Differences:

- Upstream uses an additive priority system that distinguishes "implied" from
  "explicit"; we apply user `--info=FLAG` after the verbose mapping in
  `crates/cli/src/frontend/execution/drive/options.rs:145-153` by calling
  `logging::apply_info_flag` per token, which is equivalent in the common case
  but does not preserve a separate priority for `--info=help` reporting (we
  do not implement `make_output_option`, so server-side option forwarding does
  not deduplicate implied entries).
- Upstream `do_stats` (`--stats`) parses `verbose > 1 ? "stats3" : "stats2"`
  at `options.c:2046-2049`. Our `--stats` flag at
  `crates/cli/src/frontend/arguments/parser/mod.rs:435` is a plain boolean and
  is not promoted to level 3 under `-vv`; the renderer in
  `crates/cli/src/frontend/execution/drive/summary.rs` ignores the level
  distinction.
- Upstream `limit_output_verbosity` at `options.c:527-553` clamps levels to
  the running verbose ceiling so that `--info=FLIST3` with `-v` is reduced to
  `FLIST2`. We do not implement this clamp; per-flag caps in
  `flags/info.rs:124-156` reject only the parser-level overflow.
- Upstream `negate_output_levels` and `reset_output_levels`
  (`options.c:555-578`) are used for daemon-side suppression. We have no
  equivalent because our daemon does not pipe info levels through the option
  forwarding stage.
- The `-q`/`--quiet` interaction in upstream zeros `verbose` (`options.c:1095`
  via the `case 'q'` setter, not shown above) before
  `set_output_verbosity` runs. Our parser at
  `crates/cli/src/frontend/arguments/parser/mod.rs:566-568` sets `verbosity =
  0` on `--quiet`, matching upstream behaviour.

## Summary of parity gaps

1. Per-flag emission gating is absent for DEL, FLIST, MISC, REMOVE,
   SKIP. BACKUP is wired through `backup_existing_entry` in
   `crates/engine/src/local_copy/context_impl/state.rs`; NONREG is wired
   through `record_skipped_non_regular` in
   `crates/engine/src/local_copy/context_impl/reporting.rs`; COPY is wired
   through `try_reference_dest` in
   `crates/transfer/src/receiver/quick_check.rs` and the alt-base override
   path in `crates/engine/src/local_copy/executor/file/copy/transfer/execute.rs`;
   MOUNT is wired through the cross-device skip sites in
   `crates/engine/src/local_copy/executor/directory/recursive/entry.rs` and
   `crates/engine/src/local_copy/executor/sources/orchestration.rs`;
   SYMSAFE is wired through the sender-side walker in
   `crates/transfer/src/generator/file_list/walk.rs`, the local-copy
   dereference branch in
   `crates/engine/src/local_copy/executor/special/symlink.rs`, and the
   cross-device backup fallback in
   `crates/engine/src/local_copy/context_impl/state.rs`.
   Adding `info_gte(InfoFlag::*, n)` guards at the upstream call sites
   listed above is required to graduate each remaining flag from "stub"
   or "parsed-only" to "match". Suggested insertion crates:
   `crates/engine/src/generator/*` (FLIST, SKIP),
   `crates/transfer/src/file_list/*` (FLIST),
   `crates/core/src/client/remote/*` (REMOVE),
   `crates/protocol/src/multiplex/*` and `crates/batch/src/*` (MISC).
2. STATS is collapsed into a boolean. The renderer in
   `crates/cli/src/frontend/execution/drive/summary.rs` should consult the
   numeric level to mirror upstream's progressive `STATS, 1/2/3` output.
3. NAME is parsed into an enum; restoring a numeric path keeps the
   `INFO_GTE(NAME, 1/2)` shape for downstream consumers and aligns the wire
   `make_output_option` reproduction.
4. `--info=help` output should add the per-verbosity summary block to match
   upstream's `output_item_help`.
5. Implicit additions when `--progress` or `-P` are set (`FLIST2,PROGRESS`
   and conditional `name`) are not applied; this affects user-visible defaults
   on push pulls without explicit `--info=NAME`.
6. `limit_output_verbosity` clamping is missing; explicit `--info=FLIST3` is
   accepted under `-v0` even though upstream would clamp it to 0.
7. `make_output_option` is not implemented; remote command forwarding of
   user-priority info flags relies on raw passthrough rather than upstream's
   deduplicated `--info=ALL2,NONREG0,...` style.

Daemon parity: upstream `loadparm.c` does not configure info levels per
module, so no work is required there. Our `crates/daemon/src/*` correctly
inherits the client-side info levels via the message pipeline; daemon-side
suppression of info messages on `--server` is upstream's responsibility and
runs through `parse_output_words` priority rules at
`options.c:2929,540-552`, which we have not modelled.
