# `--stats` block - upstream rsync 3.4.1 byte-level parity audit

Branch: `docs/stats-block-2111`. Audit date: 2026-05-06.

## Scope

Byte-for-byte audit of every line emitted by oc-rsync when `--stats` is enabled,
compared against upstream rsync 3.4.1. The block is consumed verbatim by CI
scripts, log-grep heuristics, and reporting harnesses, so any drift in label
text, separator characters, suffix selection, or trailing punctuation is a
visible regression.

Scope covers:

- The `INFO_GTE(STATS, 2)` block (verbose stats, lines 418-449 of
  `target/interop/upstream-src/rsync-3.4.1/main.c`).
- The `INFO_GTE(STATS, 1)` block (totals + speedup, lines 451-461).
- Number formatting via `do_big_num` / `do_big_dnum` /
  `get_number_separator` in `target/interop/upstream-src/rsync-3.4.1/lib/compat.c`.
- The `--human-readable` interaction with each label.
- Any extra lines we emit that upstream does not.

Out of scope: progress output (`--progress`), itemized output (`--itemize`),
the malloc / heap statistics block (`MEM_ALLOC_INFO`).

## Source references

Upstream (`target/interop/upstream-src/rsync-3.4.1/`):

- `main.c:387-465` - `output_itemized_counts`, `bytes_per_sec_human_dnum`,
  `output_summary`.
- `lib/compat.c:25-39` - `get_number_separator`, locale probe via `"%f", 3.14`.
- `lib/compat.c:170-246` - `do_big_num` core implementation.
- `lib/compat.c:252-272` - `do_big_dnum` for fractional values.
- `inums.h:19-57` - inline wrappers `big_num`, `comma_num`, `human_num`,
  `big_dnum`, `comma_dnum`, `human_dnum`.
- `options.c:110,607,1557` - `human_readable` option counter (post-increment,
  no level cap).

oc-rsync:

- `crates/cli/src/frontend/progress/render.rs` - `emit_stats`, `emit_totals`,
  `io_backend_label`.
- `crates/cli/src/frontend/progress/format/size.rs` - `format_size`,
  `format_decimal_bytes`, `format_human_bytes`.
- `crates/cli/src/frontend/progress/format/rate.rs` - `format_summary_rate`,
  `format_human_rate`.
- `crates/cli/src/frontend/progress/format/progress.rs` -
  `format_stat_categories`.
- `crates/core/src/client/config/enums/human_readable.rs` -
  `HumanReadableMode { Disabled, Enabled, Combined }`, `is_enabled`,
  `includes_exact`.

## Number formatter primitives

| Upstream | Behaviour | oc-rsync analog |
|----------|-----------|------------------|
| `comma_num(n)` | Integer with thousands separator. Suffixes K/M/G/T/P only when `human_readable >= 2`. Separator from locale via `get_number_separator()`. | `format_decimal_bytes(n)` (called via `format_size`). Hard-codes `,` separator regardless of locale. No K/M/G suffix path until `HumanReadableMode::Enabled` (level 1 not level 2). |
| `human_num(n)` | Same as `comma_num` but suffixes activate at `human_readable >= 1`. Default `human_readable = 1`, so vanilla rsync already emits `K`/`M`/`G`. | `format_size(n, mode)` -> `format_human_bytes` when `mode.is_enabled()`. |
| `comma_dnum(d, k)` | Double formatted via `"%.*f"` then thousands-separated. Used only for `flist_buildtime` / `flist_xfertime` / speedup. Suffix only at `human_readable >= 2`. | None. We use Rust's native `{:.3}` / `{:.2}` formatters with no thousands separator and no K/M scaling. |
| `human_dnum(d, k)` | `"%.*f"` plus suffix at `human_readable >= 1`. Used for the bytes/sec field. | `format_summary_rate` -> `format_human_rate`. Returns plain `"%.2f"` for `rate < 1000`. |
| `get_number_separator()` | Probes locale: `snprintf("%f", 3.14)` -> if `'.'` present, separator is `,`; else `.`. Caches per-process. | None. Compile-time constant `,`. |

### `do_big_num` suffix scaling

`compat.c:182-205`:

- Multiplier `mult = 1000` when `human_flag == 2`, `1024` when `human_flag >= 3`.
- Suffix sequence: `K` -> `M` -> `G` -> `T` -> `P`.
- Format string: `"%.2f%c"` (two fractional digits, single-letter suffix).
- Locale-driven decimal point comes from `printf` / locale, not from
  `get_number_separator`.

oc-rsync (`format_human_bytes`, `format/size.rs:41-63`):

- Hard-coded base-1000 thresholds. No base-1024 path.
- Suffix sequence identical (`K`, `M`, `G`, `T`, `P`).
- Format `"{value:.2}{suffix}"` - same shape as upstream.
- Decimal point comes from Rust's `Display` for `f64`, which always uses
  `.` regardless of locale.

### `do_big_num` integer thousands-separation

`compat.c:230-240`:

- Walks digits right-to-left.
- Inserts `number_separator` every 3 digits *only* when `human_flag` is set.
- When `human_flag == 0` (i.e. `big_num`), no separator at all.

oc-rsync (`format_decimal_bytes`):

- Always inserts `,` every three digits, regardless of mode.
- This means oc-rsync's "decimal" path is equivalent to upstream's
  `human_flag == 1` path, not `human_flag == 0`. Upstream's `comma_num` is
  the closest analog.

## Line-by-line audit

Format used per row:

- **Label**: literal prefix bytes upstream writes.
- **Format spec**: upstream printf format and helper.
- **Our output**: oc-rsync equivalent line in `render.rs`.
- **Parity**: `OK`, `DRIFT`, or `EXTRA`.

### Block separator before stats body

Upstream: `rprintf(FCLIENT, "\n");` at `main.c:419`. Emits a single blank
line on the client only (server writes nothing). The blank line goes to
stdout when the receiver is the local client.

oc-rsync: blank line is emitted by the caller of `emit_stats` in
`render.rs:83-89` when there is preceding listing or progress output, but
`emit_stats` itself does **not** unconditionally print a leading newline.
**Parity: DRIFT.** Upstream always inserts a blank line before the stats
block (when `INFO_GTE(STATS, 2)`); oc-rsync only inserts one if a previous
block was rendered.

### 1. Number of files

Upstream label: `Number of files`.

Format (`main.c:420`, via `output_itemized_counts` at lines 387-407):

```
Number of files: <comma_num(total)>%s
```

The `%s` is the breakdown buffer. When non-empty:

```
 (reg: <n>, dir: <n>, link: <n>, dev: <n>, special: <n>)
```

Sub-categories appear in fixed order `reg, dir, link, dev, special`. Only
non-zero categories appear, joined by `, `. The buffer opens with `" ("` and
closes with `")"`. Each entry uses `comma_num` (locale-separated when
`human_readable`).

oc-rsync (`render.rs:259`):

```
Number of files: {total_entries}{files_breakdown}
```

`files_breakdown` is built by `format_stat_categories` with categories
`reg, dir, link, special`. There is **no `dev` slot**: device totals are
folded into `special_total = devices_total + fifos_total` and rendered under
the `special` label.

`{total_entries}` comes from `format!("{}", n)` with no separator.

Parity: **DRIFT** (multiple issues):

- Missing `dev` category - upstream prints `dev: N` separately when there
  are device files; oc-rsync collapses devices into `special`.
- Total uses no thousands separator; upstream uses `comma_num` (which is
  comma-separated even at `human_readable == 0`).
- Sub-counts use plain `{count}`; upstream uses `comma_num(counts[j])`.

### 2. Number of created files

Upstream (`main.c:422`, only when `protocol_version >= 29`):

```
Number of created files: <comma_num(total)>%s
```

Same breakdown rules as line 1, but driven by `stats.created_files[]`
which is itself populated only when protocol >= 29.

oc-rsync (`render.rs:260-263`): unconditional. Uses categories
`reg, dir, link, special` (no `dev`).

Parity: **DRIFT**:

- Always emitted; upstream gates on `protocol_version >= 29`.
- Same `dev`-vs-`special` collapse as line 1.
- No thousands separator on totals or sub-counts.

### 3. Number of deleted files

Upstream (`main.c:424`, only when `protocol_version >= 31`):

```
Number of deleted files: <comma_num(total)>%s
```

Breakdown by `reg, dir, link, dev, special`. Populated by the receiver
parsing `NDX_DEL_STATS` during the goodbye phase.

oc-rsync (`render.rs:264`):

```
Number of deleted files: {deleted}
```

Parity: **DRIFT**:

- Unconditional, no protocol gate.
- No sub-category breakdown at all - upstream emits the same
  `(reg: ..., dir: ..., ...)` block when delete stats are non-zero.
- No thousands separator.

### 4. Number of regular files transferred

Upstream (`main.c:425-426`):

```
Number of regular files transferred: <comma_num(stats.xferred_files)>
```

oc-rsync (`render.rs:265`):

```
Number of regular files transferred: {files}
```

Parity: **DRIFT**. Upstream uses `comma_num` (thousands-separated even at
`human_readable == 0`); oc-rsync emits the bare integer.

### 5. Total file size

Upstream (`main.c:427-428`):

```
Total file size: <human_num(stats.total_size)> bytes
```

`human_num` -> `do_big_num(num, human_readable, NULL)`. Default
`human_readable == 1` so the field is comma-grouped without unit suffixes;
`-h` (level 2) introduces K/M/G/T/P suffix.

oc-rsync (`render.rs:266`):

```
Total file size: {total_size_display} bytes
```

`format_size(n, mode)`:

- `mode == Disabled` -> `format_decimal_bytes` (always comma-grouped).
- `mode == Enabled` -> `format_human_bytes` (suffixed).
- `mode == Combined` -> `"<human> (<decimal>)"`.

Parity: **DRIFT**:

- Default `Disabled` produces comma-grouped output, matching upstream's
  `human_readable == 1` default. So in practice the *bytes* line is OK at
  default verbosity.
- `--human-readable` (level 1) on oc-rsync triggers suffix output. On
  upstream, level 1 is already the default. Single `-h` on upstream
  bumps to level 2 (suffixed). oc-rsync needs `-h` to enable suffix
  output. **Off-by-one in `--human-readable` semantics.**
- oc-rsync's `Combined` ("level 2") emits both forms; upstream has no
  such mode.

### 6. Total transferred file size

Upstream (`main.c:429-430`):

```
Total transferred file size: <human_num(stats.total_transferred_size)> bytes
```

oc-rsync (`render.rs:267-270`):

```
Total transferred file size: {transferred_size_display} bytes
```

Parity: **OK label**, same `--human-readable` off-by-one as line 5.

### 7. Literal data

Upstream (`main.c:431-432`):

```
Literal data: <human_num(stats.literal_data)> bytes
```

oc-rsync (`render.rs:271`):

```
Literal data: {literal_bytes_display} bytes
```

Parity: **OK label**, same `--human-readable` off-by-one.

### 8. Matched data

Upstream (`main.c:433-434`):

```
Matched data: <human_num(stats.matched_data)> bytes
```

oc-rsync (`render.rs:272`):

```
Matched data: {matched_bytes_display} bytes
```

Parity: **OK label**, same `--human-readable` off-by-one.

### 9. File list size

Upstream (`main.c:435-436`):

```
File list size: <human_num(stats.flist_size)>
```

Note: no trailing ` bytes` suffix on this line (in contrast to the four
prior lines). The label ends at `<value>` with only a newline after.

oc-rsync (`render.rs:273`):

```
File list size: {file_list_size_display}
```

Parity: **OK** (label and lack of `bytes` trailer match upstream), same
`--human-readable` off-by-one.

### 10. File list generation time

Upstream (`main.c:437-440`, only when `stats.flist_buildtime != 0`):

```
File list generation time: <comma_dnum(buildtime / 1000.0, 3)> seconds
```

`comma_dnum` -> `do_big_dnum(d, human_readable != 0, 3)`. Format
`"%.3f"`, then post-processed with thousands-separator on the integer
portion if `human_readable` is set.

oc-rsync (`render.rs:274-277`): unconditional.

```
File list generation time: {file_list_generation:.3} seconds
```

Uses Rust's `f64` `Display` with `:.3`. No thousands separator on the
integer part, no locale awareness, decimal point always `.`.

Parity: **DRIFT**:

- Always emitted; upstream skips the line when `flist_buildtime == 0`.
- No thousands separator (upstream applies one for `human_readable >= 1`,
  which is default).
- Locale-insensitive decimal point.

### 11. File list transfer time

Upstream (`main.c:441-443`, same `if (stats.flist_buildtime)` gate as
line 10 - shares the conditional with line 10):

```
File list transfer time: <comma_dnum(xfertime / 1000.0, 3)> seconds
```

oc-rsync (`render.rs:278-281`): unconditional, same `:.3` formatter.

Parity: **DRIFT** (same issues as line 10).

### 12. Total bytes sent

Upstream (`main.c:445-446`):

```
Total bytes sent: <human_num(total_written)>
```

No trailing ` bytes` suffix (note distinction from line 5).

oc-rsync (`render.rs:282`):

```
Total bytes sent: {bytes_sent_display}
```

Parity: **OK** (label, lack of `bytes` trailer), `--human-readable`
off-by-one.

### 13. Total bytes received

Upstream (`main.c:447-448`):

```
Total bytes received: <human_num(total_read)>
```

oc-rsync (`render.rs:283`):

```
Total bytes received: {bytes_received_display}
```

Parity: **OK**, same `--human-readable` off-by-one.

### EXTRA: I/O backend

oc-rsync (`render.rs:284`):

```
I/O backend: {io_backend_label()}
```

Where `io_backend_label()` returns one of `"standard I/O"`, `"io_uring"`,
or `"io_uring (SQPOLL)"`.

Upstream emits no such line. This is a non-upstream addition that scripts
parsing the stats block for byte-equivalent output will treat as garbage.

Parity: **EXTRA - drift.** Should be removed or gated behind an
oc-rsync-specific verbosity / debug flag.

### Block separator before totals

Upstream (`main.c:452`): `rprintf(FCLIENT, "\n");` between the stats body
and the totals. Always emits a blank line at this point when
`INFO_GTE(STATS, 1)`.

oc-rsync: writes `writeln!(stdout)?` at `render.rs:285` after the I/O
backend line. The blank line is unconditional in `emit_stats`, so the
separator does land before the totals - but it currently follows the
spurious "I/O backend" line, not the upstream final line ("Total bytes
received").

Parity: **OK** (blank line present), but only because of the extra line.
Removing "I/O backend" preserves the parity intentionally.

### 14. sent/received/rate one-liner

Upstream (`main.c:453-456`):

```
sent <human_num(total_written)> bytes  received <human_num(total_read)> bytes  <human_dnum((written+read) / (0.5 + (endtime - starttime)), 2)> bytes/sec
```

Bytes-per-second uses `human_dnum`, fractional digits = 2. Note the
**double space** (`U+0020 U+0020`) between segments.

`bytes_per_sec_human_dnum()` (`main.c:409-414`) returns the literal
string `"UNKNOWN"` if `starttime` or `endtime` is `(time_t)-1`. The
divisor is `0.5 + (endtime - starttime)` - half-second offset to avoid
division-by-zero and to round upward at boundary cases.

oc-rsync (`render.rs:319-322`):

```
sent {sent_display} bytes  received {received_display} bytes  {rate_display} bytes/sec
```

`rate = (sent + received) as f64 / seconds`, with `seconds` from
`elapsed.as_secs_f64()`. When `seconds <= 0.0`, rate is `0.0` (not
`"UNKNOWN"`).

`format_summary_rate(rate, mode)`:

- `mode == Disabled` -> `format!("{rate:.2}")` (no thousands separator).
- `mode == Enabled` -> `format_human_rate(rate)`:
  - `< 1_000.0` -> `format!("{rate:.2}")` (raw).
  - `>= 1_000.0` -> `format!("{value:.2}{suffix}")` with suffix sequence
    `K, M, G, T, P` (base 1000).

Parity: **DRIFT**:

- Double-space between segments: matches upstream (literal `"  "` in both
  format strings). **OK.**
- Thousands separator on byte counts: upstream uses `human_num` (default
  level 1 -> separated). oc-rsync's `Disabled` mode also separates via
  `format_decimal_bytes`. **OK at default.**
- Rate field: upstream's `human_dnum` at default level 1 produces a
  *thousands-separated* `"%.2f"`. oc-rsync's `Disabled` produces plain
  `"%.2f"` with no separator. **DRIFT** when the rate exceeds 1000.
- "UNKNOWN" rate handling: oc-rsync emits `0.00` instead of `UNKNOWN`
  when elapsed is zero. **DRIFT.**
- The half-second offset (`0.5 + (endtime - starttime)`) is missing in
  oc-rsync. For sub-second transfers our rate is higher than upstream
  by a small factor. **DRIFT.**

### 15. total size / speedup / dry-run trailer

Upstream (`main.c:457-460`):

```
total size is <human_num(stats.total_size)>  speedup is <comma_dnum(total_size / (written+read), 2)>[ (BATCH ONLY)| (DRY RUN)]
```

Trailer suffix:

- `" (BATCH ONLY)"` when `write_batch < 0`.
- `" (DRY RUN)"` when `dry_run` (and not `write_batch < 0`).
- empty otherwise.

`speedup` is `comma_dnum(... , 2)` - thousands-separated `"%.2f"` at
default `human_readable >= 1`.

Note: division by `(written + read)` can be zero. Upstream produces
`inf` or `nan` printed via `printf` in that case (no guard).

oc-rsync (`render.rs:323-327`):

```
total size is {total_size_display}  speedup is {speedup:.2}{dry_run_suffix}
```

Where `speedup` is `0.0` when `transmitted == 0` (guarded), and
`dry_run_suffix` is either `" (DRY RUN)"` or empty.

Parity: **DRIFT**:

- No `(BATCH ONLY)` branch. We always print `(DRY RUN)` if `dry_run`,
  but never `(BATCH ONLY)`.
- speedup field uses Rust's `:.2` (no thousands separator).
- `transmitted == 0` -> we print `0.00`; upstream prints `inf` /
  `nan` / locale-specific text.
- Double-space between `<size>` and `speedup is` matches upstream.

## `--human-readable` interaction matrix

Default rsync behaviour (no `-h`): `human_readable = 1`. Single `-h`
increments to 2; `-hh` to 3 (base-1024).

oc-rsync default: `HumanReadableMode::Disabled` (level 0). Single `-h`
sets `Enabled`; `-h -h` sets `Combined`.

| Field family | Upstream level 0 | Upstream level 1 (default) | Upstream level 2 (`-h`) | Upstream level 3 (`-hh`) | oc-rsync `Disabled` | oc-rsync `Enabled` (`-h`) | oc-rsync `Combined` (`-hh`) |
|--------------|------------------|----------------------------|-------------------------|--------------------------|---------------------|---------------------------|-----------------------------|
| `comma_num` integers (counts) | bare digits | comma-grouped | comma-grouped | comma-grouped | bare digits | bare digits | bare digits |
| `human_num` bytes | bare digits | comma-grouped | suffixed K/M/G (base 1000) | suffixed K/M/G (base 1024) | comma-grouped | suffixed (base 1000) | "suffixed (decimal)" |
| `comma_dnum` (flist times, speedup) | `"%.*f"` only | grouped `"%.*f"` | grouped `"%.*f"` | grouped `"%.*f"` | bare `"%.*f"` | bare `"%.*f"` | bare `"%.*f"` |
| `human_dnum` (bytes/sec) | `"%.*f"` | grouped `"%.*f"` | suffixed K/M/G (base 1000) | suffixed K/M/G (base 1024) | bare `"%.*f"` | suffixed (base 1000) | "suffixed (decimal)" |

Off-by-one summary: every oc-rsync mode is one level *behind* upstream.
A user running upstream with no flags sees the same output that an
oc-rsync user only gets with `-h`. Users who pass `-h` to oc-rsync get
upstream's `-hh` semantics (modulo our base-1000-only suffix path).

## Locale handling

Upstream `get_number_separator()` (`lib/compat.c:27-39`) probes the C
locale by formatting `3.14` with `"%f"`. If the rendered string contains
`'.'`, the **thousands** separator becomes `','`; otherwise (locales
where the decimal mark is `,`, e.g. de_DE) it becomes `'.'`. The
decimal point character returned by `get_decimal_point()`
(`lib/compat.c:41-44`) is the inverse.

oc-rsync uses Rust's `f64` `Display` exclusively, which always renders
`.` as the decimal point. `format_decimal_bytes` hard-codes `,` as the
thousands separator. There is no locale probe.

Concrete user-visible drift in a `LANG=de_DE.UTF-8` environment:

- Upstream produces `1.234.567,89` for a million-byte float.
- oc-rsync produces `1,234,567.89`.

Parity: **DRIFT** for any non-C locale. The CLI does not currently
expose a knob to opt into locale-aware formatting.

## Conditional emission summary

| Upstream condition | oc-rsync behaviour | Parity |
|--------------------|--------------------|--------|
| Body emitted only when `INFO_GTE(STATS, 2)` | `emit_stats` runs whenever `stats == true` (single threshold) | OK in practice; `--stats` toggles both |
| Lead blank line `rprintf(FCLIENT, "\n")` | Conditional in caller, dependent on prior output | DRIFT |
| `Number of created files` only when protocol >= 29 | Always emitted | DRIFT |
| `Number of deleted files` only when protocol >= 31 | Always emitted | DRIFT |
| `File list generation/transfer time` only when `flist_buildtime != 0` | Always emitted | DRIFT |
| Trailer `(BATCH ONLY)` vs `(DRY RUN)` | Only `(DRY RUN)` ever printed | DRIFT |

## Discrepancy index

Counting each distinct line-level deviation:

1. Lead blank line emission rule.
2. `Number of files` - missing `dev:` sub-category.
3. `Number of files` - no thousands separator on totals/sub-counts.
4. `Number of created files` - no protocol >= 29 gate.
5. `Number of created files` - missing `dev:` sub-category.
6. `Number of created files` - no thousands separator.
7. `Number of deleted files` - no protocol >= 31 gate.
8. `Number of deleted files` - missing `(reg/dir/link/dev/special)` breakdown.
9. `Number of deleted files` - no thousands separator.
10. `Number of regular files transferred` - no thousands separator.
11. `--human-readable` off-by-one across the byte-formatter family.
12. `File list generation time` - no `flist_buildtime != 0` gate.
13. `File list generation time` - no thousands separator on integer part.
14. `File list transfer time` - no `flist_buildtime != 0` gate.
15. `File list transfer time` - no thousands separator.
16. `I/O backend` line - extra, non-upstream.
17. Bytes/sec - "UNKNOWN" sentinel missing.
18. Bytes/sec - half-second divisor offset missing.
19. Bytes/sec - no thousands separator on rate value at default.
20. `total size is ... speedup is ...` - speedup has no thousands separator.
21. Trailer - `(BATCH ONLY)` branch missing.
22. Locale-aware `get_number_separator` not implemented.

Total discrepancies: **22**.

## Recommendations

In rough priority order (highest user-visible drift first):

1. Remove the `I/O backend:` line from the stats body, or move it behind a
   debug-only verbosity that does not collide with `--stats` consumers.
2. Switch byte/count formatting defaults so oc-rsync's level 0
   (`Disabled`) maps to upstream's level 1 (current behaviour matches
   only by coincidence on the byte-count rows). Reserve `--human-readable`
   for explicit suffix output as upstream does.
3. Apply thousands-separator across all `comma_num` / `comma_dnum`
   call-site equivalents (counts, speedup, flist times, rate at default).
4. Gate `Number of created files` on protocol >= 29, `Number of deleted
   files` on protocol >= 31, and the flist-time pair on
   `flist_buildtime != 0`.
5. Restore the `dev` sub-category in the `output_itemized_counts` analog;
   add the deleted-files breakdown.
6. Implement the `(BATCH ONLY)` trailer alongside `(DRY RUN)`.
7. Match `bytes_per_sec_human_dnum` semantics: emit the literal string
   `UNKNOWN` when timing is unavailable and apply the `0.5 +`
   half-second offset.
8. Implement a `get_number_separator` analog that probes
   `nl_langinfo(THOUSEP)` / locale to keep parity in non-C locales.

The first six are mechanical text changes inside `render.rs` and
`format/`. Items 7-8 require touching `ClientSummary` accounting and
adding a locale probe respectively.
