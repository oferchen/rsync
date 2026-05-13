# `--stats` output format byte-for-byte audit

Tracking issue: oc-rsync task #2111.

Audit date: 2026-05-13.

## Scope

Byte-for-byte comparison of every line emitted by oc-rsync when `--stats` is
active against upstream rsync 3.4.1. Two output sites are audited:

1. **`protocol` crate** - `TransferStats::fmt()` (`crates/protocol/src/stats/display.rs`),
   used when stats are relayed across the wire or rendered as a standalone block.
2. **`cli` crate** - `emit_stats()` and `emit_totals()` (`crates/cli/src/frontend/progress/render.rs`),
   the primary user-facing stats renderer.

Both must match upstream's `output_summary()` (`main.c:416-465`).

## Upstream source references

- `main.c:387-407` - `output_itemized_counts()` helper.
- `main.c:409-414` - `bytes_per_sec_human_dnum()`.
- `main.c:416-465` - `output_summary()`.
- `rsync.h:1033-1047` - `struct stats` definition.
- `inums.h:19-57` - `big_num`, `comma_num`, `human_num`, `big_dnum`,
  `comma_dnum`, `human_dnum` inline wrappers.
- `lib/compat.c:25-39` - `get_number_separator()` locale probe.
- `lib/compat.c:170-272` - `do_big_num()` and `do_big_dnum()` core formatters.
- `options.c:110` - `int human_readable = 1;` (default).
- `log.c:889-892` - daemon FLOG stats line.

## oc-rsync source references

- `crates/protocol/src/stats/display.rs` - `TransferStats` `Display` impl.
- `crates/protocol/src/stats/transfer.rs` - `TransferStats` struct.
- `crates/protocol/src/stats/delete.rs` - `DeleteStats` struct.
- `crates/cli/src/frontend/progress/render.rs` - `emit_stats()`, `emit_totals()`.
- `crates/cli/src/frontend/progress/format/size.rs` - `format_size()`,
  `format_decimal_bytes()`, `format_human_bytes()`.
- `crates/cli/src/frontend/progress/format/rate.rs` - `format_summary_rate()`,
  `format_human_rate()`.
- `crates/cli/src/frontend/progress/format/progress.rs` - `format_stat_categories()`.
- `crates/core/src/client/config/enums/human_readable.rs` - `HumanReadableMode`.

## Number formatting primitives

### Upstream

Upstream uses four tiers of number formatting, all driven by the
`human_readable` counter (default 1):

| Function | `human_readable == 0` | `== 1` (default) | `== 2` (`-h`) | `>= 3` (`-hh`) |
|----------|-----------------------|-------------------|---------------|-----------------|
| `big_num(n)` | bare digits | bare digits | bare digits | bare digits |
| `comma_num(n)` | bare digits | comma-grouped | comma-grouped | comma-grouped |
| `human_num(n)` | bare digits | comma-grouped | K/M/G/T/P (base 1000) | K/M/G/T/P (base 1024) |
| `comma_dnum(d,k)` | `"%.*f"` | grouped `"%.*f"` | grouped `"%.*f"` | grouped `"%.*f"` |
| `human_dnum(d,k)` | `"%.*f"` | grouped `"%.*f"` | K/M/G/T/P (base 1000) | K/M/G/T/P (base 1024) |

The thousands separator comes from `get_number_separator()`, which probes the
C locale by formatting `3.14` via `snprintf`: if the output contains `.`, the
separator is `,`; otherwise `.` (for locales like `de_DE` where the decimal
mark is `,`).

### oc-rsync

oc-rsync has `HumanReadableMode` with three levels:

| Mode | Mapping | Behaviour |
|------|---------|-----------|
| `Disabled` (default) | Upstream level 0 | `format_decimal_bytes()` - always comma-grouped |
| `Enabled` (`-h`) | Upstream level 2 | `format_human_bytes()` - K/M/G/T/P suffix (base 1000) |
| `Combined` (`-hh`) | No upstream analog | `"<human> (<decimal>)"` |

The thousands separator is hard-coded as `,`. No locale probe.

### Key differences

1. **Off-by-one in levels.** Upstream's default (`human_readable=1`) already
   comma-groups numbers. oc-rsync's default (`Disabled`) also comma-groups
   via `format_decimal_bytes()`, so at default settings the byte output is
   similar. However, the semantics diverge for `-h`: upstream goes to suffix
   mode at `-h` (level 2); oc-rsync goes to suffix mode at `-h` (level 1).

2. **No base-1024 path.** Upstream's `-hh` (level 3) uses 1024-based units.
   oc-rsync has no base-1024 mode; `Combined` mode is unrelated.

3. **`comma_num` vs bare digits.** Upstream uses `comma_num` for integer
   counts (file counts, xferred_files), which comma-groups at
   `human_readable >= 1` (the default). oc-rsync's `emit_stats()` uses plain
   `{count}` (no separator) for all count fields. The `TransferStats::Display`
   impl in the protocol crate uses `format_number()` (comma-grouped) for counts.

4. **`comma_dnum` not implemented.** Upstream's time and speedup fields use
   `comma_dnum` which comma-groups the integer part. oc-rsync uses Rust's
   `{:.3}` / `{:.2}` with no grouping.

5. **Locale independence.** oc-rsync always uses `.` for the decimal point and
   `,` for the thousands separator, regardless of locale.

## Line-by-line audit

### Notation

- **Upstream format**: the C format string and helper function used in upstream.
- **oc-rsync format (cli)**: format produced by `emit_stats()` / `emit_totals()` in `render.rs`.
- **oc-rsync format (proto)**: format produced by `TransferStats::fmt()` in `display.rs`.
- **Match?**: `OK`, `DRIFT`, or `EXTRA`.

---

### Block separator (leading blank line)

| | Detail |
|-|--------|
| **Upstream** | `rprintf(FCLIENT, "\n")` at `main.c:419`, unconditional when `INFO_GTE(STATS, 2)` |
| **oc-rsync (cli)** | Emitted conditionally by caller in `render.rs:83-89` only when prior output exists |
| **oc-rsync (proto)** | Not emitted (starts with first stat line) |
| **Match?** | **DRIFT** - upstream always inserts a blank line before the stats block |

---

### Line 1: Number of files

| | Detail |
|-|--------|
| **Upstream** | `"Number of files: %s%s\n"` via `output_itemized_counts()` (`main.c:420`) |
| **Upstream format** | Total: `comma_num(total)`. Breakdown: `" (reg: %s, dir: %s, link: %s, dev: %s, special: %s)"` - only non-zero categories shown, `comma_num` for each |
| **oc-rsync (cli)** | `"Number of files: {total_entries}{files_breakdown}"` (`render.rs:259`). Total: `{total_entries}` via plain `Display`, no separator. Breakdown via `format_stat_categories(&[("reg", ..), ("dir", ..), ("link", ..), ("special", ..)])` - no `dev` category, plain `{count}` |
| **oc-rsync (proto)** | `"Number of files: %s"` via `format_number(self.num_files)` (comma-grouped). Breakdown includes `reg, dir, link, dev, special` with `format_number()` |
| **Match?** | **DRIFT** (cli); **DRIFT** (proto - close but `format_number` does not respect `human_readable` flag) |

Cli-specific drifts:
- Missing `dev` sub-category (devices merged into `special`).
- Total and sub-counts lack thousands separators.

Proto-specific drifts:
- `format_number` always comma-groups (matching upstream `comma_num` at default). Close to OK.
- No locale-aware separator.

---

### Line 2: Number of created files

| | Detail |
|-|--------|
| **Upstream** | `"Number of created files: %s%s\n"` via `output_itemized_counts()` (`main.c:422`). Only emitted when `protocol_version >= 29` |
| **Upstream format** | Same `comma_num` + breakdown as Line 1, using `stats.created_files[]` |
| **oc-rsync (cli)** | `"Number of created files: {created_total}{created_breakdown}"` (`render.rs:260-263`). Always emitted. Categories: `reg, dir, link, special` (no `dev`). No thousands separator on total |
| **oc-rsync (proto)** | `"Number of created files: %s\n"` via `format_number(self.num_created_files)`. Only total, no sub-category breakdown. Only emitted when `num_created_files > 0` |
| **Match?** | **DRIFT** (both) |

Drifts:
- No protocol >= 29 gate (cli - always emitted; proto - gated on nonzero count instead).
- cli: missing `dev` sub-category, no thousands separator on total.
- proto: no breakdown at all; upstream always prints the breakdown.

---

### Line 3: Number of deleted files

| | Detail |
|-|--------|
| **Upstream** | `"Number of deleted files: %s%s\n"` via `output_itemized_counts()` (`main.c:424`). Only emitted when `protocol_version >= 31` |
| **Upstream format** | Same `comma_num` + breakdown (`reg, dir, link, dev, special`) using `stats.deleted_files[]` |
| **oc-rsync (cli)** | `"Number of deleted files: {deleted}"` (`render.rs:264`). Always emitted. No sub-category breakdown. No thousands separator |
| **oc-rsync (proto)** | `"Number of deleted files: %s\n"` via `format_number(self.num_deleted_files)`. Only emitted when `num_deleted_files > 0`. No breakdown |
| **Match?** | **DRIFT** (both) |

Drifts:
- No protocol >= 31 gate.
- No `(reg: N, dir: N, link: N, dev: N, special: N)` breakdown.
- cli: no thousands separator.

---

### Line 4: Number of regular files transferred

| | Detail |
|-|--------|
| **Upstream** | `"Number of regular files transferred: %s\n"` (`main.c:425-426`). Uses `comma_num(stats.xferred_files)` |
| **oc-rsync (cli)** | `"Number of regular files transferred: {files}"` (`render.rs:265`). Plain `Display`, no separator |
| **oc-rsync (proto)** | `"Number of regular files transferred: %s\n"` via `format_number(self.num_transferred_files)`. Only emitted when `num_transferred_files > 0` |
| **Match?** | **DRIFT** (cli - no thousands separator); **DRIFT** (proto - conditional on nonzero, comma-grouped) |

---

### Line 5: Total file size

| | Detail |
|-|--------|
| **Upstream** | `"Total file size: %s bytes\n"` (`main.c:427-428`). Uses `human_num(stats.total_size)` |
| **oc-rsync (cli)** | `"Total file size: {total_size_display} bytes"` (`render.rs:266`). Uses `format_size(n, mode)` |
| **oc-rsync (proto)** | `"Total file size: %s bytes\n"` via `format_number(self.total_size)`. Only emitted when `total_size > 0` |
| **Match?** | **DRIFT** (both - see notes) |

Notes:
- Upstream uses `human_num` which at default (`human_readable=1`) comma-groups.
  oc-rsync cli with default `Disabled` also comma-groups via `format_decimal_bytes`.
  **Byte-level output matches at default settings.**
- Off-by-one in `-h` levels: upstream `-h` -> suffix; oc-rsync `-h` -> suffix. Actually aligned here.
- Proto: conditional emission (only when > 0); upstream always emits.
- Proto uses `format_number` (always comma-grouped) - ignores `human_readable`.

---

### Line 6: Total transferred file size

| | Detail |
|-|--------|
| **Upstream** | `"Total transferred file size: %s bytes\n"` (`main.c:429-430`). Uses `human_num(stats.total_transferred_size)` |
| **oc-rsync (cli)** | `"Total transferred file size: {transferred_size_display} bytes"` (`render.rs:267-270`). Uses `format_size()` |
| **oc-rsync (proto)** | `"Total transferred file size: %s bytes\n"` via `format_number(self.total_transferred_size)`. Only when `total_transferred_size > 0` |
| **Match?** | **OK** (cli label + format at default); **DRIFT** (proto conditional) |

---

### Line 7: Literal data

| | Detail |
|-|--------|
| **Upstream** | `"Literal data: %s bytes\n"` (`main.c:431-432`). Uses `human_num(stats.literal_data)` |
| **oc-rsync (cli)** | `"Literal data: {literal_bytes_display} bytes"` (`render.rs:271`). Uses `format_size()` |
| **oc-rsync (proto)** | `"Literal data: %s bytes\n"` via `format_number(self.literal_data)`. Only when `total_transferred_size > 0 \|\| literal_data > 0 \|\| matched_data > 0` |
| **Match?** | **OK** (cli label); **DRIFT** (proto conditional) |

---

### Line 8: Matched data

| | Detail |
|-|--------|
| **Upstream** | `"Matched data: %s bytes\n"` (`main.c:433-434`). Uses `human_num(stats.matched_data)` |
| **oc-rsync (cli)** | `"Matched data: {matched_bytes_display} bytes"` (`render.rs:272`). Uses `format_size()` |
| **oc-rsync (proto)** | `"Matched data: %s bytes\n"` via `format_number(self.matched_data)`. Same conditional as Line 7 |
| **Match?** | **OK** (cli label); **DRIFT** (proto conditional) |

---

### Line 9: File list size

| | Detail |
|-|--------|
| **Upstream** | `"File list size: %s\n"` (`main.c:435-436`). Uses `human_num(stats.flist_size)`. **No trailing ` bytes`** |
| **oc-rsync (cli)** | `"File list size: {file_list_size_display}"` (`render.rs:273`). Uses `format_size()`. No trailing ` bytes` |
| **oc-rsync (proto)** | `"File list size: %s\n"` via `format_number(self.flist_size)`. Only when `flist_size > 0`. No trailing ` bytes` |
| **Match?** | **OK** (both label and lack of ` bytes` suffix match) |

---

### Line 10: File list generation time

| | Detail |
|-|--------|
| **Upstream** | `"File list generation time: %s seconds\n"` (`main.c:438-440`). Uses `comma_dnum(buildtime / 1000.0, 3)`. Only emitted when `stats.flist_buildtime != 0` |
| **oc-rsync (cli)** | `"File list generation time: {file_list_generation:.3} seconds"` (`render.rs:274-277`). Always emitted. Divides by `1_000_000.0` (microseconds). No thousands separator on integer part |
| **oc-rsync (proto)** | `"File list generation time: {secs:.3} seconds"` (`display.rs:147`). Only when `flist_buildtime > 0`. Divides by `1_000_000.0` |
| **Match?** | **DRIFT** (both) |

Drifts:
- cli: always emitted; upstream gates on `flist_buildtime != 0`.
- Both: no thousands separator on the integer portion (upstream `comma_dnum`
  groups at `human_readable >= 1`).
- Time unit: upstream stores milliseconds and divides by 1000; oc-rsync stores
  microseconds and divides by 1,000,000. Both produce seconds - **OK** if values
  match, but the wire format exchange (protocol crate) sends milliseconds
  matching upstream.

---

### Line 11: File list transfer time

| | Detail |
|-|--------|
| **Upstream** | `"File list transfer time: %s seconds\n"` (`main.c:441-443`). Uses `comma_dnum(xfertime / 1000.0, 3)`. Shares the same `if (stats.flist_buildtime)` gate as Line 10 |
| **oc-rsync (cli)** | `"File list transfer time: {file_list_transfer:.3} seconds"` (`render.rs:278-281`). Always emitted |
| **oc-rsync (proto)** | `"File list transfer time: {secs:.3} seconds"` (`display.rs:152`). Only when `flist_xfertime > 0` (different gate than upstream) |
| **Match?** | **DRIFT** (both - same issues as Line 10) |

---

### Line 12: Total bytes sent

| | Detail |
|-|--------|
| **Upstream** | `"Total bytes sent: %s\n"` (`main.c:445-446`). Uses `human_num(total_written)`. **No trailing ` bytes`** |
| **oc-rsync (cli)** | `"Total bytes sent: {bytes_sent_display}"` (`render.rs:282`). Uses `format_size()`. No trailing ` bytes` |
| **oc-rsync (proto)** | `"Total bytes sent: %s\n"` via `format_number(self.total_written)`. No trailing ` bytes` |
| **Match?** | **OK** (label, no ` bytes` suffix) |

---

### Line 13: Total bytes received

| | Detail |
|-|--------|
| **Upstream** | `"Total bytes received: %s\n"` (`main.c:447-448`). Uses `human_num(total_read)`. **No trailing ` bytes`** |
| **oc-rsync (cli)** | `"Total bytes received: {bytes_received_display}"` (`render.rs:283`). Uses `format_size()`. No trailing ` bytes` |
| **oc-rsync (proto)** | `"Total bytes received: %s\n"` via `format_number(self.total_read)`. No trailing ` bytes` |
| **Match?** | **OK** (label, no ` bytes` suffix) |

---

### EXTRA: I/O backend

| | Detail |
|-|--------|
| **Upstream** | Not present |
| **oc-rsync (cli)** | `"I/O backend: {io_backend_label()}"` (`render.rs:284`). Values: `"standard I/O"`, `"io_uring"`, `"io_uring (SQPOLL)"` |
| **oc-rsync (proto)** | Not present |
| **Match?** | **EXTRA** - non-upstream line in the stats block |

This line will cause any script or test that parses the stats block for
byte-equivalent output to see an unexpected line.

---

### Block separator (between stats body and totals)

| | Detail |
|-|--------|
| **Upstream** | `rprintf(FCLIENT, "\n")` at `main.c:452`, unconditional when `INFO_GTE(STATS, 1)` |
| **oc-rsync (cli)** | `writeln!(stdout)?` at `render.rs:285` (unconditional in `emit_stats`) |
| **oc-rsync (proto)** | Not applicable (totals are part of the same `Display` output) |
| **Match?** | **OK** (blank line present, but follows the extra I/O backend line) |

---

### Line 14: sent/received/rate one-liner

| | Detail |
|-|--------|
| **Upstream** | `"sent %s bytes  received %s bytes  %s bytes/sec\n"` (`main.c:453-456`). `human_num` for byte counts, `bytes_per_sec_human_dnum()` for rate. Note double-space between segments |
| **Upstream rate** | `human_dnum((written+read) / (0.5 + (endtime - starttime)), 2)`. Returns `"UNKNOWN"` if timing unavailable. Half-second offset prevents division-by-zero |
| **oc-rsync (cli)** | `"sent {sent_display} bytes  received {received_display} bytes  {rate_display} bytes/sec"` (`render.rs:319-322`). `format_size()` for byte counts, `format_summary_rate()` for rate. Double-space present |
| **oc-rsync (proto)** | `"sent %s bytes  received %s bytes  %.2f bytes/sec"` (`display.rs:167-173`). `format_number()` for byte counts. `bytes_per_sec()` divides by flist times, not wall clock. Double-space present |
| **Match?** | **DRIFT** (both) |

Cli-specific drifts:
- Rate uses wall-clock elapsed with no half-second offset (upstream uses
  `0.5 + (endtime - starttime)`).
- When elapsed is zero, rate is `0.00` instead of upstream's `"UNKNOWN"`.
- Default mode: rate formatted via `format!("{rate:.2}")` with no thousands
  separator; upstream `human_dnum` at level 1 comma-groups the integer part.

Proto-specific drifts:
- Rate calculated from `flist_buildtime + flist_xfertime`, not wall-clock time.
- Always outputs `0.00` when times are zero, never `"UNKNOWN"`.
- `format_number()` always comma-groups byte counts (close to upstream default).

---

### Line 15: total size / speedup / trailer

| | Detail |
|-|--------|
| **Upstream** | `"total size is %s  speedup is %s%s\n"` (`main.c:457-460`). `human_num` for total size, `comma_dnum(total_size / (written+read), 2)` for speedup. Trailer: `" (BATCH ONLY)"` when `write_batch < 0`, `" (DRY RUN)"` when `dry_run`, empty otherwise. Note double-space before `speedup` |
| **oc-rsync (cli)** | `"total size is {total_size_display}  speedup is {speedup:.2}{dry_run_suffix}"` (`render.rs:325-327`). `format_size()` for total size. `speedup` via `f64 / f64` with guard for zero. `dry_run_suffix`: `" (DRY RUN)"` or empty. Double-space present |
| **oc-rsync (proto)** | `"total size is %s  speedup is %.2f"` (`display.rs:176-181`). `format_number()` for size. No DRY RUN / BATCH trailer. Double-space present |
| **Match?** | **DRIFT** (both) |

Cli-specific drifts:
- No `(BATCH ONLY)` trailer (only `(DRY RUN)` implemented).
- Speedup has no thousands separator (upstream `comma_dnum` groups at
  `human_readable >= 1`).
- Division-by-zero guarded to produce `0.00`; upstream produces `inf`/`nan`
  via `printf`.

Proto-specific drifts:
- No trailer at all (no dry_run or batch info available in protocol stats).
- No thousands separator on speedup.

---

## Summary table

| # | Line | Upstream format | oc-rsync cli format | oc-rsync proto format | Cli match? | Proto match? | Notes |
|---|------|----------------|--------------------|-----------------------|-----------|-------------|-------|
| - | Lead blank line | `\n` (unconditional) | conditional | not emitted | DRIFT | DRIFT | |
| 1 | Number of files | `comma_num(total)` + `(reg/dir/link/dev/special)` | `{n}` + `(reg/dir/link/special)` | `format_number` + `(reg/dir/link/dev/special)` | DRIFT | DRIFT | cli: no `dev`, no separators |
| 2 | Number of created files | `comma_num` + breakdown, proto >= 29 | `{n}` + `(reg/dir/link/special)`, unconditional | `format_number`, total only, > 0 gate | DRIFT | DRIFT | no proto gate; cli: no `dev` |
| 3 | Number of deleted files | `comma_num` + breakdown, proto >= 31 | `{n}`, no breakdown, unconditional | `format_number`, total only, > 0 gate | DRIFT | DRIFT | no proto gate; no breakdown |
| 4 | Regular files transferred | `comma_num` | `{n}` | `format_number`, > 0 gate | DRIFT | DRIFT | cli: no separator |
| 5 | Total file size | `human_num` + ` bytes` | `format_size` + ` bytes` | `format_number` + ` bytes`, > 0 gate | OK | DRIFT | at default settings |
| 6 | Total transferred file size | `human_num` + ` bytes` | `format_size` + ` bytes` | `format_number` + ` bytes`, > 0 gate | OK | DRIFT | |
| 7 | Literal data | `human_num` + ` bytes` | `format_size` + ` bytes` | `format_number` + ` bytes`, conditional | OK | DRIFT | |
| 8 | Matched data | `human_num` + ` bytes` | `format_size` + ` bytes` | `format_number` + ` bytes`, conditional | OK | DRIFT | |
| 9 | File list size | `human_num` (no ` bytes`) | `format_size` (no ` bytes`) | `format_number` (no ` bytes`), > 0 gate | OK | DRIFT | |
| 10 | File list generation time | `comma_dnum(.../1000, 3)`, gated | `{:.3}`, always emitted | `{:.3}`, > 0 gate | DRIFT | DRIFT | no thousands sep; different gate |
| 11 | File list transfer time | `comma_dnum(.../1000, 3)`, gated | `{:.3}`, always emitted | `{:.3}`, > 0 gate | DRIFT | DRIFT | shares upstream's `buildtime` gate |
| 12 | Total bytes sent | `human_num` (no ` bytes`) | `format_size` (no ` bytes`) | `format_number` (no ` bytes`) | OK | OK | |
| 13 | Total bytes received | `human_num` (no ` bytes`) | `format_size` (no ` bytes`) | `format_number` (no ` bytes`) | OK | OK | |
| - | I/O backend | not present | `I/O backend: {label}` | not present | EXTRA | N/A | non-upstream line |
| - | Separator blank line | `\n` (unconditional) | `\n` (unconditional) | N/A | OK | N/A | |
| 14 | sent/received/rate | `human_num` + `human_dnum(.., 2)` | `format_size` + `format_summary_rate` | `format_number` + `{:.2}` | DRIFT | DRIFT | rate calculation differs |
| 15 | total size/speedup/trailer | `human_num` + `comma_dnum(.., 2)` + trailer | `format_size` + `{:.2}` + DRY RUN only | `format_number` + `{:.2}`, no trailer | DRIFT | DRIFT | no BATCH ONLY; no sep on speedup |

## Conditional emission audit

Upstream gates certain lines on protocol version. oc-rsync does not:

| Upstream condition | oc-rsync (cli) | oc-rsync (proto) | Parity |
|--------------------|----------------|------------------|--------|
| Stats body: `INFO_GTE(STATS, 2)` | `stats == true` | `Display` always renders | OK |
| Lead `\n` unconditional | Conditional on prior output | Not emitted | DRIFT |
| `Number of created files`: proto >= 29 | Always emitted | Emitted when > 0 | DRIFT |
| `Number of deleted files`: proto >= 31 | Always emitted | Emitted when > 0 | DRIFT |
| `File list gen/xfer time`: `flist_buildtime != 0` | Always emitted | Each gated on own value > 0 | DRIFT |
| Totals: `INFO_GTE(STATS, 1)` | `stats \|\| verbosity > 0` | Always rendered | OK |
| Trailer `(BATCH ONLY)` when `write_batch < 0` | Never emitted | Never emitted | DRIFT |
| Trailer `(DRY RUN)` when `dry_run` | Emitted | Not available | DRIFT (proto only) |

## `--human-readable` interaction audit

| oc-rsync `-h` level | oc-rsync behaviour | Upstream equivalent | Notes |
|---------------------|--------------------|---------------------|-------|
| 0 (default, `Disabled`) | `format_decimal_bytes`: comma-grouped | Upstream level 1 (default): comma-grouped | Output matches at default, but semantic levels are offset |
| 1 (`-h`, `Enabled`) | `format_human_bytes`: K/M/G/T/P base-1000 suffix | Upstream level 2 (`-h`): K/M/G/T/P base-1000 suffix | Output matches for single `-h` |
| 2 (`-hh`, `Combined`) | `"<human> (<decimal>)"` | No upstream equivalent | Upstream level 3 (`-hh`): base-1024 suffix |

Integer counts (`comma_num` call sites in upstream) are affected differently:

| Field type | Upstream (default) | oc-rsync cli (default) | oc-rsync proto |
|-----------|-------------------|----------------------|----------------|
| File counts (Lines 1-4) | comma-grouped | bare digits | comma-grouped (`format_number`) |
| Byte values (Lines 5-9, 12-13) | comma-grouped | comma-grouped | comma-grouped |
| Time values (Lines 10-11) | comma-grouped fractional | bare fractional | bare fractional |
| Rate (Line 14) | comma-grouped fractional | bare fractional | bare fractional |
| Speedup (Line 15) | comma-grouped fractional | bare fractional | bare fractional |

## Daemon log line

Upstream emits a daemon-only log line via `rprintf(FLOG, ...)` in `log.c:889`:

```
sent <big_num(written)> bytes  received <big_num(read)> bytes  total size <big_num(total_size)>
```

This uses `big_num` (never comma-grouped, regardless of `human_readable`).
oc-rsync does not currently emit a daemon log equivalent.

**Match: N/A** - not applicable until daemon FLOG stats are implemented.

## Discrepancy index

Unique discrepancies across both output sites:

| # | Description | Affects |
|---|-------------|---------|
| 1 | Lead blank line emission rule | cli |
| 2 | `Number of files` - missing `dev` sub-category | cli |
| 3 | `Number of files` - no thousands separator on total/sub-counts | cli |
| 4 | `Number of created files` - no protocol >= 29 gate | cli, proto |
| 5 | `Number of created files` - missing `dev` sub-category | cli |
| 6 | `Number of created files` - no sub-category breakdown | proto |
| 7 | `Number of created files` - no thousands separator | cli |
| 8 | `Number of deleted files` - no protocol >= 31 gate | cli, proto |
| 9 | `Number of deleted files` - no `(reg/dir/link/dev/special)` breakdown | cli, proto |
| 10 | `Number of deleted files` - no thousands separator | cli |
| 11 | `Number of regular files transferred` - no thousands separator | cli |
| 12 | `File list generation time` - no `flist_buildtime != 0` gate | cli |
| 13 | `File list generation time` - no thousands separator on integer part | cli, proto |
| 14 | `File list transfer time` - no `flist_buildtime != 0` gate (shares upstream gate) | cli |
| 15 | `File list transfer time` - proto gates on own value instead of buildtime | proto |
| 16 | `I/O backend` line - extra, non-upstream | cli |
| 17 | Bytes/sec rate - no `"UNKNOWN"` sentinel when timing unavailable | cli, proto |
| 18 | Bytes/sec rate - no half-second divisor offset | cli |
| 19 | Bytes/sec rate - proto uses flist times, not wall-clock elapsed | proto |
| 20 | Bytes/sec rate - no thousands separator at default | cli |
| 21 | Speedup - no thousands separator | cli, proto |
| 22 | Trailer - `(BATCH ONLY)` branch missing | cli |
| 23 | Trailer - not available in proto `Display` | proto |
| 24 | Division-by-zero guard: `0.00` instead of upstream `inf`/`nan` | cli, proto |
| 25 | No base-1024 path for `-hh` | cli |
| 26 | Locale-aware `get_number_separator` not implemented | cli, proto |
| 27 | Proto `Display`: conditional emission of lines that upstream always prints | proto |

Total: **27** distinct discrepancies.

## Recommendations

Ordered by user-visible impact:

1. **Remove `I/O backend` line** from the stats block. Move it behind a
   debug-only flag or `INFO_GTE(STATS, 3)` equivalent so it does not pollute
   the standard `--stats` output that scripts and interop tests parse.

2. **Add thousands separators to integer counts** in `emit_stats()`. File
   counts (Lines 1-4) should use `format_decimal_bytes()` or a dedicated
   integer formatter with commas, matching upstream's `comma_num` behaviour at
   default `human_readable = 1`.

3. **Restore `dev` sub-category** in `format_stat_categories` calls. Upstream
   prints five categories (`reg, dir, link, dev, special`); oc-rsync cli merges
   `dev` into `special`. Track device counts separately and add the `dev` slot.

4. **Add deleted-files breakdown.** Use the existing `DeleteStats` struct
   (which already tracks `files, dirs, symlinks, devices, specials`) to render
   the `(reg: N, dir: N, ...)` breakdown for the "Number of deleted files" line.

5. **Gate conditional lines on protocol version.** `Number of created files`
   should only appear when protocol >= 29; `Number of deleted files` when
   protocol >= 31; flist time pair when `flist_buildtime != 0`.

6. **Add thousands separator to fractional fields.** The `comma_dnum` analog
   should group the integer portion of the time and speedup fields. Implement a
   `format_grouped_float(value, decimal_digits)` helper that inserts commas
   into the integer part.

7. **Implement `(BATCH ONLY)` trailer** alongside `(DRY RUN)` on Line 15.

8. **Match rate semantics.** Emit `"UNKNOWN"` when timing is unavailable.
   Apply the `0.5 + elapsed` half-second offset. Fix the proto crate's
   `bytes_per_sec()` to use wall-clock time instead of flist times.

9. **Proto `Display`: align conditional emission** with upstream. Lines 1-13
   should always be printed (even when values are zero), matching upstream's
   unconditional output within the stats block.

10. **Locale probe.** Implement a `get_number_separator` analog that uses
    `nl_langinfo(THOUSEP)` or a libc `snprintf` probe on Unix to determine the
    thousands separator and decimal point. Low priority - affects non-English
    locales only.

11. **Base-1024 path.** Add a `HumanReadableMode::Binary` variant or similar
    to support upstream's `-hh` (level 3, base-1024) formatting. Low priority -
    the base-1000 path covers the common case.
