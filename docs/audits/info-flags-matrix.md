# --info=FLAGS verbosity matrix vs upstream

Tracking issue: #2112. Companion to `docs/audits/info-flags-audit.md` (deeper
gating analysis); this file is the canonical family-by-family matrix and the
test action plan.

## 1. Upstream `info_words[]`

Source: `target/interop/upstream-src/rsync-3.4.1/options.c:228-285`.

`info_verbosity[]` (lines 239-243) drives the cumulative `-v` mapping; level 0
implies `NONREG`, level 1 adds `COPY,DEL,FLIST,MISC,NAME,STATS,SYMSAFE`, level
2 adds `BACKUP,MISC2,MOUNT,NAME2,REMOVE,SKIP`. `info_words[]` (lines 270-285)
is the parser table consulted by `parse_output_words` and rendered by
`output_item_help` (`options.c:474-510`). All thirteen `INFO_*` constants live
in `rsync.h:1416-1435`.

| Family   | Where bits     | Upstream max level   | Help string (verbatim)                                       |
| -------- | -------------- | -------------------- | ------------------------------------------------------------ |
| BACKUP   | `W_REC`        | 1                    | Mention files backed up                                      |
| COPY     | `W_REC`        | 1                    | Mention files copied locally on the receiving side           |
| DEL      | `W_REC`        | 1                    | Mention deletions on the receiving side                      |
| FLIST    | `W_CLI`        | 2                    | Mention file-list receiving/sending (levels 1-2)             |
| MISC     | `W_SND\|W_REC` | 2                    | Mention miscellaneous information (levels 1-2)               |
| MOUNT    | `W_SND\|W_REC` | 1                    | Mention mounts that were found or skipped                    |
| NAME     | `W_SND\|W_REC` | 2                    | Mention 1) updated file/dir names, 2) unchanged names        |
| NONREG   | `W_REC`        | 1 (default 1)        | Mention skipped non-regular files (default 1, 0 disables)    |
| PROGRESS | `W_CLI`        | 2                    | Mention 1) per-file progress or 2) total transfer progress   |
| REMOVE   | `W_SND`        | 1                    | Mention files removed on the sending side                    |
| SKIP     | `W_REC`        | 2                    | Mention files skipped due to transfer overrides (levels 1-2) |
| STATS    | `W_CLI\|W_SRV` | 3                    | Mention statistics at end of run (levels 1-3)                |
| SYMSAFE  | `W_SND\|W_REC` | 1                    | Mention symlinks that are unsafe                             |

## 2. Level suffix and `--info=help`

`parse_output_words` (`options.c:380-470`) treats a trailing digit as the
level: `--info=FLIST2` is `FLIST` at level 2; bare `--info=FLIST` defaults to
1; `--info=FLIST0` clears the flag. Bare `--info=help` triggers
`output_item_help`, which prints every `info_words[]` row, then `ALL`,
`NONE`, `HELP`, then a per-verbosity summary block built from
`info_verbosity[]`. The leading line is `Use OPT or OPT1 for level 1 output,
OPT2 for level 2, etc.; OPT0 silences.`

## 3. oc-rsync implementation surface

- Token table and per-flag caps:
  `crates/cli/src/frontend/execution/flags/info.rs:75-160`.
- Secondary parser (numeric levels, `none`/`all`/`help`):
  `crates/cli/src/frontend/info_output.rs:283-441`.
- Per-flag storage: `crates/logging/src/levels/info.rs` (`InfoFlag`,
  `InfoLevels::{get,set,set_all}`).
- `-v` mapping: `crates/logging/src/config.rs:43-195`
  (`VerbosityConfig::from_verbose_level`).
- Drive-time merging of clap output and tokenised `--info=`:
  `crates/cli/src/frontend/execution/drive/options.rs:123-153`.
- Help text: `INFO_HELP_TEXT` at
  `crates/cli/src/frontend/execution/flags/info.rs:228-246`.

## 4. Family parity matrix

| Family   | Token parsed | Level cap | Stored in `InfoLevels` | Production gating site | Status      |
| -------- | ------------ | --------- | ---------------------- | ---------------------- | ----------- |
| BACKUP   | yes          | none      | yes                    | none                   | stub        |
| COPY     | yes          | none      | yes                    | none                   | stub        |
| DEL      | yes          | none      | yes                    | none                   | stub        |
| FLIST    | yes          | 2         | yes                    | none                   | parsed-only |
| MISC     | yes          | 2         | yes                    | none                   | parsed-only |
| MOUNT    | yes          | none      | yes                    | none                   | stub        |
| NAME     | yes          | enum      | numeric+enum           | progress/itemize       | partial     |
| NONREG   | yes          | none      | yes                    | none                   | stub        |
| PROGRESS | yes          | 2         | yes                    | progress renderer      | match       |
| REMOVE   | yes          | none      | yes                    | none                   | stub        |
| SKIP     | yes          | 2         | yes                    | none                   | parsed-only |
| STATS    | yes          | 3         | bool collapse          | summary renderer       | partial     |
| SYMSAFE  | yes          | none      | yes                    | none                   | stub        |

Legend: **match** parses and gates production output; **partial** parses but
collapses level shape or misses some upstream sites; **parsed-only** stores
the level but no producer consults it; **stub** stores the level and the
production callers do not gate on it (no upper cap either). No family is
silently dropped or wrongly named: every upstream token is accepted.

Cross-cutting deviations (full breakdown in `info-flags-audit.md`):
`limit_output_verbosity` clamping is missing; `--progress` does not auto-add
`FLIST2,PROGRESS`; `--stats` is not promoted to `STATS3` under `-vv`;
`make_output_option` (server arg forwarding) is unimplemented; `--info=help`
omits the per-verbosity summary block and the leading "Use OPT..." line.

## 5. Action plan

1. **Golden test for `--info=help`** under `crates/cli/tests/`. Capture
   upstream `rsync --info=help` output verbatim from
   `target/interop/upstream-src/rsync-3.4.1/rsync` and assert byte equality
   against our `INFO_HELP_TEXT`. Track gaps (per-verbosity block, leading
   line) as fixes that close the assertion delta. Use the existing
   `tests/golden/` harness pattern.
2. **Per-family integration tests** at `tests/info_flag_matrix.rs`. One test
   per family: invoke `oc-rsync --info=FAMILY -nv src/ dst/` against a
   fixture that exercises the upstream-emitting code path (backup file, copy,
   delete, flist, mount cross, non-regular, remove-source-files, skipped
   transfer, symlink unsafe), run upstream rsync against the same fixture,
   and assert that emitted lines for that family appear/disappear at level 0,
   1, and the documented max level. Lights a red bulb for every "stub" and
   "parsed-only" cell above; flips green as gates land.
3. **Numeric suffix coverage** in `crates/cli/tests/info_flag_levels.rs`.
   Parametrise over `(flag, level)` for level 0, 1, max, max+1, asserting
   parser accept/reject parity with upstream including the level cap (FLIST2
   ok, FLIST3 errors; STATS3 ok, STATS4 errors).
4. **`--info=NONE` / `--info=ALL` / `--info=help` parser parity** as a
   separate test; ensure `none` zeros every level and `all` raises every
   level to its declared max (matching `output_item_help`'s `ALL` row).
5. **Verbose-level mapping** test: `oc-rsync -v` and `-vv` must seed
   `InfoLevels` exactly per `info_verbosity[]`; covered partially by
   `crates/logging/tests/info_flag_parsing.rs`, extend to a full table-driven
   case for levels 0-5.
