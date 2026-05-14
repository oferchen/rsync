# Progress Line Format Audit

## Scope

Byte-for-byte comparison of `--progress` and `--info=progress2` output between
upstream rsync 3.4.1 and our `oc-rsync` implementation.

Sources audited:

- Upstream: `target/interop/upstream-src/rsync-3.4.1/progress.c`
- Upstream: `target/interop/upstream-src/rsync-3.4.1/lib/compat.c` (`do_big_num`)
- Upstream: `target/interop/upstream-src/rsync-3.4.1/main.c` (`output_summary`)
- Ours: `crates/cli/src/frontend/progress/render.rs`
- Ours: `crates/cli/src/frontend/progress/live.rs`
- Ours: `crates/cli/src/frontend/progress/format/progress.rs`
- Ours: `crates/cli/src/frontend/progress/format/rate.rs`
- Ours: `crates/cli/src/frontend/progress/format/size.rs`

The reference `printf` template is `progress.c:129-130`:

```
rprintf(FCLIENT, "\r%15s %3d%% %7.2f%s %s%s",
    human_num(ofs), pct, rate, units, rembuf, eol);
```

## 1. Per-File Progress Line Format

### Upstream layout

| Field | C format | Width | Notes |
|-------|----------|-------|-------|
| Bytes transferred | `%15s` | 15, right-aligned, space pad | `human_num(ofs)`; with `--human-readable >= 2` switches to `1.23K`/`M`/`G`/`T`/`P` (`compat.c:182-205`); otherwise grouped digits with locale separator (`compat.c:230-240`) |
| Percent | `%3d%%` | 3 + literal `%` | `pct = ofs == size ? 100 : (int)(100.0 * ofs / size)` (`progress.c:128`) |
| Rate value | `%7.2f` | 7, two fractional digits | `rate` in kB/s, MB/s, or GB/s based on threshold |
| Rate unit | `%s` | variable | `kB/s`, `MB/s`, `GB/s` (`progress.c:108-116`) |
| ETA / total | `%s` | fixed via `snprintf("%4u:%02u:%02u")` | `??:??:??` placeholder when `remain < 0` or `> 9999h` (`progress.c:118-125`) |
| Tail | `%s` | variable | `eol`: trailing `"  "` for in-flight lines (`progress.c:100`) or `" (xfr#N, to-chk=I/J)\n"` / `" (xfr#N, ir-chk=I/J)\n"` for final updates (`progress.c:78-82`) |

The leading byte is always `\r` (carriage return; `progress.c:129`). The rsync
binary writes through `rprintf(FCLIENT, ...)`, which routes to stdout for the
client (`log.c:288` and downstream).

### Our layout (`live.rs:88-106` and `render.rs:175-194`)

```
{size_field} {percent_field} {rate_field} {elapsed_field} (xfr#{xfr_index}, to-chk={remaining}/{total})
```

| Field | Rust format | Width | Notes |
|-------|-------------|-------|-------|
| Bytes transferred | `format!("{:>15}", format_progress_bytes(...))` | 15 | Decimal grouping with `,` (`size.rs:27-39`) or human-readable `1.50K`/`M`/`G`/`T`/`P` (`size.rs:41-63`) |
| Percent | `format!("{percent:>4}")` | **4**, includes the trailing `%` (e.g. ` 50%`) | `format_progress_percent` returns `"<n>%"` or `"??%"` (`progress.rs:7-17`) |
| Rate | `format!("{:>12}", format_progress_rate(...))` | 12, right-aligned | `"1.23kB/s"`/`"1.23MB/s"`/`"1.23GB/s"` (`rate.rs:136-148`); zero-or-zero-elapsed produces `"0.00kB/s"` |
| Elapsed | `format!("{:>11}", format_progress_elapsed(...))` | 11, right-aligned | `H:MM:SS` (`progress.rs:20-26`); zero hours render as a single digit, e.g. `0:00:45` |
| Tail | `(xfr#{xfr_index}, to-chk={remaining}/{total})` | variable | Always the `to-chk` form; never `ir-chk` |

### Discrepancies

1. **Percent field width**: upstream is exactly 3 columns (`%3d%%` plus literal `%`), so the percent column occupies 4 visible characters with a numeric width of 3. Ours pads to 4 columns including the `%`, so single-digit percentages render as `_ _5%` (one extra space) and `100%` matches by accident. Examples:
   - Upstream `5%`: `"  5%"` (3 + `%`).
   - Ours `5%`: `"  5%"` matches by accident; `50%` renders as ` 50%` with three leading spaces vs upstream's two.
2. **Rate field width**: upstream emits `%7.2f%s` -> exactly 7 columns of value plus the unit (`kB/s` / `MB/s` / `GB/s`); ours pads the combined `value+unit` to 12 columns. For `"1.23MB/s"` upstream produces `"   1.23MB/s"` (4 leading spaces; total 11 chars) while ours produces `"    1.23MB/s"` (12 chars). Misalignment is one column wider.
3. **Rate unit set**: upstream uses only `kB/s`, `MB/s`, `GB/s` (`progress.c:108-116`). Ours additionally produces `TB/s` and `PB/s` via `format_progress_rate_human` -> `format_verbose_rate_human` (`rate.rs:80-97`). Mixed-base units never appear upstream.
4. **Rate divisor**: upstream uses 1024-based thresholds, switching at `> 1024` and `> 1024*1024` kB/s (`progress.c:108-111`). Our decimal path uses 1024-based KiB/MiB/GiB (`rate.rs:137-148`), but the `format_progress_rate_human` path used in human-readable mode falls back to 1000-based units (`rate.rs:81-87`), which disagrees with upstream's strictly binary scaling.
5. **ETA vs elapsed** (RESOLVED): upstream prints **time remaining** in the in-flight line (`progress.c:105`) and switches to **total time taken** for the last line (`progress.c:97-98`). The `??:??:??` literal appears when ETA is unknown (`progress.c:118-119`). `live.rs` now drives a `RemainingTimeEstimator` (`format/remaining.rs`) for the mid-transfer ticks and only falls back to elapsed for the final tick.
6. **ETA width** (RESOLVED): upstream uses `%4u:%02u:%02u` -> minimum 10 columns (4 + 1 + 2 + 1 + 2). `RemainingTimeEstimator::render` emits `H:MM:SS` plus the `??:??:??` placeholder, right-padded to 10 in the live renderer.
7. **Trailing tail (in-flight)**: upstream sets `eol = "  "` (two spaces) on each live tick (`progress.c:100`) so the next `\r` overwrites cleanly. Ours emits the full `(xfr#N, to-chk=R/T)` tail on every tick (`live.rs:103-106`), not just the final update.
8. **Trailing tail (final)**: upstream uses `to-chk` only while the file list is still streaming (`!flist_eof`) and switches to `ir-chk` once the flist is complete (`progress.c:80`). Ours hard-codes `to-chk` (`live.rs:106`, `render.rs:192`).
9. **xfr#N and counts**: upstream's `xfr#N` is `stats.xferred_files` (regular files transferred to date) and the chk count is `stats.num_files - current_file_index - 1` (`progress.c:79-82`). Ours uses `update.index()` (count of progress events emitted, `progress.rs:215-217`) and the local remaining/total derived from event order (`live.rs:69-71`). For runs that include directories, symlinks, or skipped entries the totals will diverge from upstream's reg-only `xferred_files`.
10. **Leading newline before per-file line**: upstream stays on a single line per file by emitting `\r` and overwriting in place (`progress.c:129`). Ours emits `writeln!` for the path on a separate line, then writes the progress fields with no leading `\r` for the first tick (`live.rs:79-101`). The path line is upstream's `instant_progress` print (`progress.c:158-165`), but only when `!stdout_format_has_i && !INFO_GTE(NAME, 1)`.

## 2. info=progress2 Line

### Upstream behaviour (`progress.c:172-179`, `progress.c:200-203`)

When `INFO_GTE(PROGRESS, 2)`:

- `ofs = stats.total_transferred_size - size + ofs;` and `size = stats.total_size;` so the line displays cumulative totals across the whole transfer.
- `eol` final block converts the trailing newline into a padded space-erase: `last_len < --len` widens `last_len` to the longest line ever printed, then back-fills with spaces and drops the `\n` (`progress.c:84-91`). The last line is then redrawn with `is_last = 0` so `\r` is preserved and the line is not terminated with LF.
- The final `\n` is therefore deferred: progress2 leaves the cursor on the same line until `output_summary` prints the trailing newline (`main.c:419`, `main.c:452`).

### Our behaviour (`live.rs:117-150`)

`ProgressMode::Overall` reads `update.overall_transferred()`, `update.overall_total_bytes()`, and `update.overall_elapsed()` and prints the same field set as the per-file line. Final closing rule: `if update.remaining() == 0 && update.is_final()` we emit a literal `\n` (`live.rs:142-145`), otherwise we set `line_active` and let the next tick prepend `\r`.

### Discrepancies

11. **No padding to longest prior line**: upstream tracks `static int last_len` and pads the last update with spaces so leftover characters from longer earlier lines are erased (`progress.c:84-91`). We never track the longest line; if the field set ever shrinks (e.g. rate unit goes from `MB/s` -> `kB/s`) stale characters from the prior tick remain on screen.
12. **Trailing newline on final tick**: upstream explicitly drops the `\n` from `eol` for progress2 (`progress.c:88` sets `eol[last_len] = '\0'` and `is_last = 0`), so the cursor stays on the line until `output_summary` prints `"\n"`. Ours emits `writeln!(self.writer)` on the final overall tick (`live.rs:143`), forcing an immediate LF and producing an extra blank line before the summary block.
13. **Cumulative offset rebasing**: upstream rebases `ofs` to the per-transfer total (`progress.c:201`) which is what `show_progress` consumes for ratio/percent. Ours sources `update.overall_transferred()` directly from `ClientProgressForwarder` (`progress.rs:226-230` in `core/src/client/progress.rs`). The numerator agrees only when no skipped/non-regular entries exist, because upstream excludes those from `total_transferred_size` while we include any byte counter we receive.

## 3. CR vs LF (TTY foreground check)

### Upstream (`progress.c:185-238`)

- Foreground check is gated by `HAVE_GETPGRP && HAVE_TCGETPGRP`. The TTY's foreground process group is read once via `tcgetpgrp(STDOUT_FILENO)` and compared against `pgrp = getpgrp()` (`progress.c:194-195`, `progress.c:235-237`).
- If the comparison fails or `tcgetpgrp` returns -1, `show_progress` returns **before** calling `rprint_progress` (`progress.c:236-237`). No bytes are emitted while backgrounded.
- The CR (`\r`) prefix is unconditional once the line is emitted (`progress.c:129`); the LF is added only when `is_last && quiet == 0 && progress2 == 0` (`progress.c:131-134` and `eol` carries `"\n"` in the `is_last` branch).
- `output_needs_newline` is set to 1 after each tick (`progress.c:132`) so other output paths know to insert a `\n` before printing.

### Ours (`live.rs:99-114`)

- No foreground/TTY check. Progress writes go to `self.writer` whenever `--progress` is set, including when stdout is redirected to a pipe or file.
- `\r` is written only when `self.line_active` is true (i.e. on the second and subsequent ticks for the same path) (`live.rs:99-101`). The first tick has no leading `\r`.
- `\n` is written when `update.is_final()` is true (`live.rs:108-111`) and unconditionally on `finish` if the line is still active (`live.rs:55-57`).

### Discrepancies

14. **No background check**: we do not consult `tcgetpgrp` / `IsTerminal` before emitting progress, so a backgrounded `oc-rsync` continues to spam `\r` updates into a pipe or log file. Upstream silently suppresses ticks while backgrounded (`progress.c:234-237`).
15. **First-tick CR**: upstream always writes `\r` (`progress.c:129`); ours skips it on the first tick of each file. After a non-progress write to stdout the cursor will not be returned to column zero before the first per-file line.
16. **No `output_needs_newline` flag**: upstream coordinates with `log.c` so any subsequent rprintf inserts a `\n` to break out of the in-place line (`progress.c:127`, `progress.c:132`). We have no equivalent: any concurrent stderr/stdout write will smear the progress line.

## 4. 1-Second Update Throttling

### Upstream (`progress.c:182-232`)

- `show_progress` keeps a 5-slot ring (`PROGRESS_HISTORY_SECS = 5`, `progress.c:37`).
- On every call: if the newest history slot is younger than 1000 ms (`msdiff < 1000`), the function returns immediately (`progress.c:224-225`).
- If older than 1000 ms, it advances the ring by one slot (one second of state) and proceeds to render (`progress.c:227-231`).
- Initial seed: when `ph_start` is unset and the most recent receive was within 1500 ms, the start anchor copies the newest sample's timestamp (`progress.c:208-218`); otherwise it anchors to `now`.
- `end_progress` and `instant_progress` bypass the throttle (`progress.c:163`, `progress.c:172-178`).
- `want_progress_now` is a forced-tick flag set from outside and consumed by `instant_progress` (`progress.c:35`, `progress.c:158-165`).

### Ours

- No throttling is performed in `LiveProgress`; every `ClientProgressUpdate` produces output (`live.rs:64-159`). Throttling, if present, must come from upstream of the observer.
- `ClientProgressForwarder::handle_progress` (`crates/core/src/client/progress.rs:247-285`) emits an update on every callback from the engine; there is no time-based filter.

### Discrepancies

17. **1-second throttle** (RESOLVED for ETA): `RemainingTimeEstimator::observe` ignores samples that arrive within 1 s of the newest retained sample (`format/remaining.rs:60-72`), matching the `msdiff < 1000` guard in `progress.c:224-225`. The rate column still consumes raw cumulative bytes.
18. **5-slot history ring** (RESOLVED for ETA): `RemainingTimeEstimator` keeps a 5-slot ring (`HISTORY_SLOTS = PROGRESS_HISTORY_SECS = 5`) and computes the rate from the oldest retained sample to the freshest, mirroring `progress.c:102-104`. The summary rate (`format_summary_rate`) intentionally remains cumulative, matching upstream's `main.c:413` summary semantics.
19. **No `want_progress_now` / `instant_progress` equivalent**: upstream forces a single tick when external code sets the flag (e.g. before printing a non-progress line). We have no path to flush a final tick out of band.
20. **No 1500 ms backfill of the start anchor**: upstream's `ph_start` heuristic seeds from the previous file's last sample if recent (`progress.c:208-218`). Our `overall_start` is fixed at observer construction (`progress.rs:181`) and per-file elapsed comes from the engine, so the boundary case (long pause then resume) reports a low rate for the first second of the new file.

## 5. Final Summary Line ("sent X bytes ...")

### Upstream (`main.c:451-461`)

```
sent %s bytes  received %s bytes  %s bytes/sec
total size is %s  speedup is %s%s
```

- All three numbers run through `human_num()` (`main.c:455-456`) -> `do_big_num(num, human_readable, NULL)` (`inums.h:33-37`).
- `human_readable` is the option count: 0 = grouped digits, 1 = grouped digits, 2 = base-1000 SI suffix, 3 = base-1024 IEC suffix (`compat.c:177-205`). At human flag 0/1 the result is plain digits with the locale separator.
- The bytes/sec field comes from `bytes_per_sec_human_dnum()` -> `human_dnum((written + read) / (0.5 + (endtime - starttime)), 2)` (`main.c:413`), keeping two fractional digits.
- `speedup` uses `comma_dnum((double)total_size / (total_written + total_read), 2)` (`main.c:459`).
- Trailing tag: `" (BATCH ONLY)"` when `write_batch < 0`, `" (DRY RUN)"` when `dry_run`, otherwise empty (`main.c:460`).
- Two double-spaces separate the three sent/received/rate fields ("`bytes  received`", "`bytes  RATE`"). The `total size` line uses one double-space before `speedup is`.
- A blank line precedes the summary when `INFO_GTE(STATS, 1)` (`main.c:452`).

### Ours (`render.rs:319-327`)

```
sent {sent_display} bytes  received {received_display} bytes  {rate_display} bytes/sec
total size is {total_size_display}  speedup is {speedup:.2}{dry_run_suffix}
```

- `sent_display`/`received_display`/`total_size_display` -> `format_size` (`size.rs:13-25`). With human-readable disabled this is grouped digits with `,` separators (`size.rs:27-39`); enabled it uses base-1000 SI suffixes (`size.rs:46-52`).
- `rate_display` -> `format_summary_rate(rate, mode)` (`rate.rs:13-25`). Decimal mode emits `format!("{rate:.2}")` -> two fractional digits, no separator.
- `speedup` is `format!("{:.2}")` directly (`render.rs:326`).
- Dry-run suffix: `" (DRY RUN)"` only; no `(BATCH ONLY)` branch (`render.rs:323`).
- No leading blank line; the caller controls preceding whitespace.

### Discrepancies

21. **Locale separator vs comma**: upstream uses `get_number_separator()` to pick the locale's grouping character (`compat.c:177-178`, used inside `do_big_num`). We hard-code `,` in `format_decimal_bytes` (`size.rs:38`). On `LC_NUMERIC` locales that use `.` or non-breaking space upstream and ours diverge.
22. **Human-readable scale base**: upstream `human_flag == 2` is base-1000, `human_flag == 3` is base-1024 (`compat.c:182-205`). Ours has a single boolean (`HumanReadableMode::is_enabled()`) that always maps to base-1000 (`size.rs:46-63`). The `--human-readable` count of three (which upstream treats as IEC binary) is collapsed to the SI path.
23. **Rate field on summary**: upstream prints `bytes_per_sec_human_dnum()` which yields a string like `"1.23M"` or `"123,456.78"` depending on human flag (`main.c:413`, `compat.c:252-272`). Ours prints `format_summary_rate(rate, mode)` which uses K/M/G/T/P (uppercase, no trailing space) (`rate.rs:32-48`). Upstream's `do_big_dnum` only emits up to `T` for sane runtimes; ours adds `P`. The unit letter set is upper-case in ours (`K`,`M`,`G`,`T`,`P`) and matches upstream.
24. **`(BATCH ONLY)` suffix missing**: upstream emits `" (BATCH ONLY)"` when `write_batch < 0` (`main.c:460`). We never emit this (`render.rs:323`).
25. **Number of bytes/rate fractional digits**: upstream uses `do_big_dnum(..., 2)` (`main.c:413` -> `compat.c:258`) so the rate gets exactly two decimals. Ours emits `format!("{rate:.2}")` (`rate.rs:14`) which agrees on plain decimal but disagrees when the human path takes over (still 2 decimals; matches).
26. **Leading blank line**: upstream prints `"\n"` to FCLIENT before the summary at `INFO_GTE(STATS, 1)` (`main.c:452`). We rely on the caller for spacing (`render.rs:83-89`).
27. **Speedup formatting**: upstream calls `comma_dnum((double)total_size / (total_written+total_read), 2)` (`main.c:459`) which produces `1,234.56` style with grouping. Ours uses `{speedup:.2}` (`render.rs:326`) producing `1234.56` with no grouping.

## 6. Discrepancy Summary Table

| # | Area | Upstream reference | Our reference | Severity |
|---|------|---------------------|----------------|----------|
| 1 | Percent column width | `progress.c:129` `%3d%%` | `live.rs:91`, `render.rs:181` | minor |
| 2 | Rate value width | `progress.c:129` `%7.2f%s` | `live.rs:92-95`, `render.rs:182-185` | minor |
| 3 | Rate unit set | `progress.c:108-116` | `rate.rs:80-97` | major |
| 4 | Rate divisor base | `progress.c:108-111` | `rate.rs:81-87` | major |
| 5 | ETA vs elapsed | `progress.c:97-105` | `live.rs:117-124,161-168` | RESOLVED |
| 6 | ETA width | `progress.c:121-122` | `format/remaining.rs:106-114` | RESOLVED |
| 7 | In-flight tail | `progress.c:100` | `live.rs:103-106` | major |
| 8 | `to-chk` vs `ir-chk` | `progress.c:78-82` | `live.rs:106`, `render.rs:192` | major |
| 9 | xfr#N counter source | `progress.c:78-82` | `progress.rs:215-217` | major |
| 10 | First-tick path line vs CR | `progress.c:129`, `progress.c:158-165` | `live.rs:79-101` | minor |
| 11 | progress2 padding to longest line | `progress.c:84-91` | `live.rs:117-150` | major |
| 12 | progress2 final newline deferral | `progress.c:88-91`, `main.c:452` | `live.rs:142-145` | major |
| 13 | progress2 cumulative offset basis | `progress.c:200-203` | `crates/core/src/client/progress.rs:226-241` | minor |
| 14 | TTY foreground check | `progress.c:185-237` | absent | major |
| 15 | First-tick CR | `progress.c:129` | `live.rs:99-101` | minor |
| 16 | `output_needs_newline` flag | `progress.c:127`, `progress.c:132` | absent | minor |
| 17 | 1-second throttle | `progress.c:224-225` | `format/remaining.rs:60-72` | RESOLVED |
| 18 | 5-slot rolling rate window | `progress.c:102-104` | `format/remaining.rs:34-72` | RESOLVED |
| 19 | `instant_progress` / `want_progress_now` | `progress.c:35`, `progress.c:158-165` | absent | minor |
| 20 | 1500 ms start backfill | `progress.c:208-218` | `progress.rs:181` | minor |
| 21 | Locale grouping separator | `compat.c:177-178`, `compat.c:230-240` | `size.rs:38` | minor |
| 22 | Human-readable base 1000 vs 1024 | `compat.c:182-205` | `size.rs:46-63` | major |
| 23 | Summary rate unit set | `main.c:413` | `rate.rs:32-48` | minor |
| 24 | `(BATCH ONLY)` suffix | `main.c:460` | `render.rs:323` | major |
| 25 | Rate fractional digits | `compat.c:258` | `rate.rs:14` | match |
| 26 | Leading blank line | `main.c:452` | `render.rs:83-89` (caller-driven) | minor |
| 27 | Speedup grouping | `main.c:459` | `render.rs:326` | minor |

Total discrepancies recorded: **27** (one row, #25, agrees by coincidence).
Resolved to date: D5, D6, D17, D18 (sliding-window remaining-time estimator;
see `crates/cli/src/frontend/progress/format/remaining.rs`).

## 7. Verification Recipe

Save as `tools/verify_progress_format.sh` and run on a Linux host with both
binaries on `$PATH`. The script writes deterministic input, captures progress
lines under both implementations, and prints a diff of normalised columns.

```sh
#!/bin/sh
# Compares the per-file progress line emitted by upstream rsync 3.4.1 and
# oc-rsync. Run from any tmpfs-mounted scratch directory.
set -eu

UPSTREAM="${UPSTREAM_RSYNC:-rsync}"
OCRSYNC="${OC_RSYNC:-oc-rsync}"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
src="$work/src"
dst_up="$work/dst_up"
dst_oc="$work/dst_oc"
mkdir -p "$src" "$dst_up" "$dst_oc"

# 32 MiB deterministic file - large enough to trigger >1 progress tick.
dd if=/dev/zero of="$src/file.bin" bs=1M count=32 status=none

capture () {
    bin="$1"
    out="$2"
    # script(1) gives the binary a PTY so the foreground-pgrp check passes
    # and CR-separated ticks land in the log unmodified.
    script -q -c "$bin --progress --no-h '$src/' '$3/'" "$out" >/dev/null
}

capture "$UPSTREAM" "$work/upstream.log" "$dst_up"
capture "$OCRSYNC"  "$work/ocrsync.log"  "$dst_oc"

# Replace CR with LF so each tick lands on its own line, then keep only
# lines that begin with whitespace + digits (the progress rows).
normalise () {
    tr '\r' '\n' < "$1" | awk '/^[[:space:]]*[0-9]/'
}

normalise "$work/upstream.log" > "$work/upstream.norm"
normalise "$work/ocrsync.log"  > "$work/ocrsync.norm"

echo "=== upstream final tick ==="
tail -n 1 "$work/upstream.norm"
echo "=== oc-rsync final tick ==="
tail -n 1 "$work/ocrsync.norm"

echo "=== column-by-column diff (final tick) ==="
paste \
    <(tail -n 1 "$work/upstream.norm" | tr -s ' ' '\n') \
    <(tail -n 1 "$work/ocrsync.norm"  | tr -s ' ' '\n') |
    awk -F '\t' '{
        marker = ($1 == $2) ? "  " : "!="
        printf "%-24s %s %-24s\n", $1, marker, $2
    }'

echo "=== summary lines ==="
grep -E "^(sent |total size is )" "$work/upstream.log" || true
echo "---"
grep -E "^(sent |total size is )" "$work/ocrsync.log" || true
```

Expected manual verifications after running:

- The first column (bytes transferred) should be 15 wide in both.
- The second column should be `100%` for the final tick. Mid-transfer the
  upstream column is right-justified within 4 visible characters; ours uses 4.
- The third column should match the upstream `%7.2f<unit>` exactly. Discrepancy
  rows 2-4 surface here.
- The fourth column shows time. Upstream prints ETA mid-transfer and the total
  duration on the final tick. Ours prints the elapsed time on every tick
  (discrepancy 5).
- The trailing `(xfr#N, to-chk=R/T)` becomes `(xfr#N, ir-chk=R/T)` upstream
  once the file list completes; ours stays on `to-chk` (discrepancy 8).
- The summary block should contain `sent ... bytes  received ... bytes
  ... bytes/sec` and `total size is ...  speedup is ...`. The presence or
  absence of `(BATCH ONLY)` exposes discrepancy 24 when invoked with
  `--write-batch=...` and `--only-write-batch`.
