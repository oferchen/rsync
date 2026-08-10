# Audit: `--stats` block byte-for-byte format vs upstream

Tracking: oc-rsync task #2111.

## Scope

Compare the verbatim output of `--stats` (and the verbose totals trailer)
emitted by oc-rsync against upstream rsync 3.4.1 at
`target/interop/upstream-src/rsync-3.4.1`. Cover line ordering, label
spelling, thousands separators, decimal rendering, conditional gating,
and the trailing `sent ... received ... bytes/sec` /
`total size is ... speedup is ...` summary.

## 1. Upstream `--stats` output

Source: `main.c::output_summary()` lines 416-465 (block gated by
`INFO_GTE(STATS, 2)`, trailer by `INFO_GTE(STATS, 1)`). `--stats` lifts
`STATS` to level 2 (or 3 with `-vv`), per `options.c:2046-2048`. Numbers
go through `comma_num` / `human_num` / `comma_dnum` (`inums.h`,
`lib/compat.c::do_big_num`); commas appear only when `human_readable >= 1`
(default 1).

| # | Line (printf format) | Source | Gating |
|---|----------------------|--------|--------|
| 1 | `\n` (FCLIENT) | - | always at block start |
| 2 | `Number of files: %s (reg: %s, dir: %s, link: %s, dev: %s, special: %s)` | `stats.num_files` plus per-type counts; suffix omitted when total is 0; only non-zero categories listed | always |
| 3 | `Number of created files: %s (...)` | `stats.created_files` | `protocol_version >= 29` |
| 4 | `Number of deleted files: %s (...)` | `stats.deleted_files` | `protocol_version >= 31` |
| 5 | `Number of regular files transferred: %s` | `stats.xferred_files` | always |
| 6 | `Total file size: %s bytes` | `stats.total_size` | always |
| 7 | `Total transferred file size: %s bytes` | `stats.total_transferred_size` | always |
| 8 | `Literal data: %s bytes` | `stats.literal_data` | always |
| 9 | `Matched data: %s bytes` | `stats.matched_data` | always |
| 10 | `File list size: %s` | `stats.flist_size` (no `bytes` suffix) | always |
| 11 | `File list generation time: %s seconds` | `comma_dnum(ms/1000, 3)` | `flist_buildtime != 0` |
| 12 | `File list transfer time: %s seconds` | `comma_dnum(ms/1000, 3)` | same gate as 11 |
| 13 | `Total bytes sent: %s` | `total_written` | always |
| 14 | `Total bytes received: %s` | `total_read` | always |
| 15 | `\n` (FCLIENT) | - | start of `STATS,1` trailer |
| 16 | `sent %s bytes  received %s bytes  %s bytes/sec` | `total_written`, `total_read`, `human_dnum((sent+recv)/(0.5+elapsed), 2)` | `STATS >= 1` |
| 17 | `total size is %s  speedup is %s%s` | total_size, `comma_dnum(total_size/(sent+recv),2)`, optional ` (BATCH ONLY)` or ` (DRY RUN)` | `STATS >= 1` |

Lines 16 and 17 use **two literal spaces** between fields. Each line
ends with `\n`; `output_summary` finishes with `fflush(stdout)`.

## 2. oc-rsync impl

Three parallel formatters exist:

- `crates/cli/src/frontend/progress/render.rs::emit_stats` (200-288):
  live path used by `oc-rsync --stats`, sourced from `ClientSummary`.
- `crates/cli/src/frontend/stats_format.rs::StatsFormatter::format`
  (136-263): library helper used by golden tests.
- `crates/protocol/src/stats/display.rs::Display for TransferStats`
  (49-184): formatter on the protocol-side struct.

`format_number` (commas) is reimplemented in all three modules.

## 3. Field-level diff

| Field | Upstream | `emit_stats` | `stats_format` | `protocol::stats::display` |
|-------|----------|--------------|----------------|----------------------------|
| Leading `\n` | yes | **missing** | missing | missing |
| Per-type breakdown | reg, dir, link, dev, special | reg, dir, link, **special** (no dev) | none | reg, dir, link, dev, special |
| Created/Deleted lines | proto-gated (>=29 / >=31) | always | always | gated `> 0` (different) |
| Created files per-type | yes | yes | **none** | yes |
| Deleted files per-type | yes | **bare integer** | none | none |
| `flist_buildtime==0` suppresses lines 11-12 | yes | no | no | yes |
| `Total bytes sent/received` | `human_num` (commas) | `format_size`/`HumanReadableMode` | commas | commas |
| Extra `I/O backend: ...` line | absent | FIXED (removed) | absent | absent |
| Speed divisor | `0.5+(end-start)` wall-clock | wall-clock elapsed | `flist_build+flist_xfer` (**wrong**) | same wrong divisor |
| Speedup `comma_dnum(...,2)` | yes (commas) | matches | matches | `{:.2}` no commas |
| `(DRY RUN)` suffix | yes | yes | absent | absent |
| `(BATCH ONLY)` suffix | yes | **absent** | absent | absent |
| `--no-human-readable` drops commas | yes | not honored | not honored | not honored |
| Trailing `\n` after line 17 | yes | yes | **no** (`write!`) | **no** (`write!`) |

## 4. Test plan

Golden fixtures under `crates/cli/tests/fixtures/stats/` keyed by
`(protocol_version, stats_level, human_readable, dry_run, batch)`:

- Protocols 30, 31, 32 - confirms gating of created (>=29) / deleted (>=31).
- `--stats` (STATS=2), `-v --stats` (STATS=3), bare `-v` (trailer only).
- `--no-human-readable` vs default - confirms comma elision globally.
- `--dry-run` and `--write-batch` - confirms suffix on line 17.
- Empty transfer - confirms file-list timing suppression.
- Per-type breakdown matrix: only-reg; reg+dir; reg+dir+link+dev+special.

Each fixture captures the full upstream stdout under `LC_ALL=C`, stored
verbatim, then asserts `assert_eq!` against oc-rsync (not `contains`).
Add per-formatter unit tests pinning trailing `\n` and leading blank line.

## 5. Known divergences worth tracking

Status legend: FIXED in this audit; OPEN remains a divergence; CLOSED was
re-evaluated and the implementation already matches upstream.

| ID | Severity | Status | Description |
|----|----------|--------|-------------|
| S1 | Low | FIXED | `I/O backend:` line (`render.rs:288`) is not in upstream's `output_summary`; removed so the stats block ends at `Total bytes received:` and the blank line precedes the trailer as upstream emits at `main.c:452`. |
| S2 | Low | OPEN | `dev` bucket merged with `special` in `emit_stats`; upstream emits `dev` and `special` separately. |
| S3 | Low | OPEN | Deletion per-type breakdown missing; emitted as bare integer. |
| S4 | Low | OPEN | Leading blank line missing before `Number of files:` (upstream `main.c:419`). |
| S5 | Low | OPEN | Protocol-version gating absent for created (>=29) / deleted (>=31). |
| S6 | Medium | OPEN | `flist_buildtime == 0` suppression absent in `emit_stats` and `stats_format`; upstream omits both timing lines together. |
| S7 | High | OPEN | `bytes/sec` divisor wrong in `stats_format` and `protocol::stats::display` (file-list timing instead of wall-clock). |
| S8 | Low | OPEN | No trailing `\n` from `stats_format::format` and `Display for TransferStats`. |
| S9 | Medium | OPEN | `--no-human-readable` ignored on stats path (commas hardcoded). |
| S10 | Low | OPEN | Speedup uses `{:.2}` in `protocol::stats::display`, losing thousands separators upstream emits via `comma_dnum`. |
| S11 | Low | OPEN | `(BATCH ONLY)` suffix missing from `emit_stats` trailer. |
| S12 | Low | OPEN | Three duplicate formatters; consolidate into one `StatsFormatter` shared between the live path and goldens. |

## References

- Upstream: `main.c::output_summary` (416-465); `inums.h`;
  `lib/compat.c::do_big_num` (170-246), `do_big_dnum` (252-).
- oc-rsync: `crates/cli/src/frontend/progress/render.rs::emit_stats`,
  `emit_totals`;
  `crates/cli/src/frontend/stats_format.rs` (136-263);
  `crates/protocol/src/stats/display.rs` (49-184).
