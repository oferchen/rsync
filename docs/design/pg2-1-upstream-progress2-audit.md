# PG2-1: Upstream rsync progress2 Output Format Audit

Audit of upstream rsync 3.4.1 `progress.c` and supporting files, documenting the
exact behavior of `--info=progress2` (overall transfer progress) and the standard
`--progress` (per-file progress) modes. Also catalogs oc-rsync's existing
infrastructure and identifies gaps.

## 1. Upstream progress levels

Upstream uses a single `INFO_PROGRESS` integer level (options.c:279):

| Level | Mode | Triggered by |
|-------|------|-------------|
| 0 | Disabled (default) | `--no-progress` or no flag |
| 1 | Per-file progress | `--progress`, `-P`, or `--info=progress` |
| 2 | Overall transfer progress | `--info=progress2` |

`--progress` sets `do_progress = 1`. At options.c:2342-2345, if `do_progress &&
!am_server`, upstream calls `parse_output_words` with `"FLIST2,PROGRESS"` -
setting `info_levels[INFO_PROGRESS] = 1` (per-file) and `info_levels[INFO_FLIST]
= 2`. The key distinction: `--info=progress2` sets the level to 2, switching to
overall transfer progress (progress2 mode). There is no separate flag; it is the
same `INFO_PROGRESS` variable at a higher level.

`-P` is shorthand for `--progress --partial` (options.c:1600-1606).

## 2. Exact output format string

The format string at progress.c:129:

```c
rprintf(FCLIENT, "\r%15s %3d%% %7.2f%s %s%s",
        human_num(ofs), pct, rate, units, rembuf, eol);
```

### Field breakdown

| Field | Format | Width | Description |
|-------|--------|-------|-------------|
| `\r` | literal | 1 | Carriage return for in-place overwrite |
| `ofs` | `%15s` | 15 | Bytes transferred, thousands-separated via `human_num()` |
| ` ` | literal | 1 | Space separator |
| `pct` | `%3d%%` | 4 | Percentage (0-100) followed by `%` sign |
| ` ` | literal | 1 | Space separator |
| `rate` | `%7.2f` | 7 | Transfer rate numeric value |
| `units` | `%s` | 4 | Rate unit suffix: `kB/s`, `MB/s`, or `GB/s` |
| ` ` | literal | 1 | Space separator |
| `rembuf` | `%s` | 10 | Remaining time or elapsed time: `%4u:%02u:%02u` |
| `eol` | `%s` | variable | Trailing info (see below) |

### The `eol` field

For mid-transfer ticks (`is_last == 0`), `eol` is `"  "` (two spaces) -
progress.c:100.

For final ticks (`is_last == 1`), `eol` is computed at progress.c:78-82:

```c
snprintf(eol, sizeof eol,
    " (xfr#%d, %s-chk=%d/%d)\n",
    stats.xferred_files, flist_eof ? "to" : "ir",
    stats.num_files - current_file_index - 1,
    stats.num_files);
```

Format: ` (xfr#N, to-chk=R/T)\n` or ` (xfr#N, ir-chk=R/T)\n`

- `xfr#N`: 1-based count of files actually transferred (`stats.xferred_files`)
- `to-chk` vs `ir-chk`: `to` when file list is complete (`flist_eof`), `ir`
  during incremental recursion
- `R`: remaining files to check (`stats.num_files - current_file_index - 1`)
- `T`: total files (`stats.num_files`)

### progress2 final-tick padding

When `INFO_GTE(PROGRESS, 2)` (progress2 mode), the final-tick logic at
progress.c:84-91 strips the trailing `\n` from `eol` and pads with spaces so the
line never shrinks. This means progress2 never emits a newline mid-transfer - it
continuously overwrites the same line with `\r`. The line only grows or stays the
same width (never shortens), preventing visual artifacts.

## 3. Rate calculation algorithm

### Mid-transfer rate: sliding window

Upstream uses a 5-slot circular buffer (`ph_list[5]`, progress.c:37-52) to
compute the transfer rate from *recent* history, not the cumulative average:

```c
#define PROGRESS_HISTORY_SECS 5

struct progress_history {
    struct timeval time;
    OFF_T ofs;
};

static struct progress_history ph_list[PROGRESS_HISTORY_SECS];
static int newest_hpos, oldest_hpos;
```

Mid-transfer rate (progress.c:102-104):

```c
diff = msdiff(&ph_list[oldest_hpos].time, now);
rate = (double)(ofs - ph_list[oldest_hpos].ofs) * 1000.0 / diff / 1024.0;
```

The rate is computed as bytes transferred between the oldest retained sample and
the current time, divided by the elapsed milliseconds, converted to kB/s. This
gives a rate that responds to recent throughput changes within the 5-second
window.

### Final-tick rate: cumulative from start

For the final tick of a file (progress.c:94-96):

```c
diff = msdiff(&ph_start.time, now);
rate = (double)(ofs - ph_start.ofs) * 1000.0 / diff / 1024.0;
```

Uses `ph_start` (the transfer start time/offset), giving the cumulative average
rate for the entire file (or entire transfer in progress2 mode).

### Rate unit scaling

Rate is in kB/s (base-1024). Scaling at progress.c:108-116:

```c
if (rate > 1024*1024) {      // > 1 TB/s in kB/s units
    rate /= 1024.0 * 1024.0;
    units = "GB/s";
} else if (rate > 1024) {    // > 1 GB/s in kB/s units
    rate /= 1024.0;
    units = "MB/s";
} else {
    units = "kB/s";
}
```

Note: The thresholds are in kB/s space. `rate > 1024` means the raw rate exceeds
1024 kB/s (= 1 MB/s), so it scales to MB/s. `rate > 1024*1024` means it exceeds
1024 MB/s (= 1 GB/s), so it scales to GB/s. The unit never goes above GB/s -
extreme rates stay in GB/s with a large numeric value.

## 4. ETA algorithm

### Mid-transfer ETA

The ETA is computed from the sliding-window rate at progress.c:105:

```c
remain = rate ? (double)(size - ofs) / rate / 1000.0 : 0.0;
```

Where `rate` is in kB/s, so `(size - ofs)` bytes divided by `(rate * 1000)` gives
seconds. When rate is zero (stalled transfer), remain collapses to `0.0`, rendering
as `0:00:00` rather than an infinite/undefined ETA.

### Final-tick ETA

On the final tick, `remain` is replaced with the actual elapsed time
(progress.c:98):

```c
remain = (double)diff / 1000.0;
```

This switches from "estimated time remaining" to "total time taken" for the final
progress line.

### ETA rendering

Rendered at progress.c:118-125:

```c
if (remain < 0 || remain > 9999.0 * 3600.0)
    strlcpy(rembuf, "  ??:??:??", sizeof rembuf);
else {
    snprintf(rembuf, sizeof rembuf, "%4u:%02u:%02u",
             (unsigned int)(remain / 3600.0),
             (unsigned int)(remain / 60.0) % 60,
             (unsigned int)remain % 60);
}
```

- Overflow guard: values exceeding 9999 hours (35,996,400 seconds) or negative
  values render as `  ??:??:??` (10 chars including 2 leading spaces)
- Normal format: `%4u:%02u:%02u` - hours right-justified in 4 chars, colon,
  zero-padded minutes, colon, zero-padded seconds (10 chars total)

## 5. Update frequency / triggering mechanism

### Trigger: per `recv_token()` call in receiver

`show_progress()` is called from three sites in receiver.c, all guarded by
`INFO_GTE(PROGRESS, 1)`:

1. **Append mode data copying** (receiver.c:294-295, 302-303): Called per
   `CHUNK_SIZE` block while checksumming the existing portion of the file.

2. **Delta token loop** (receiver.c:316-317): Called for every token received
   from the sender - both literal data tokens (`i > 0`) and copy/match tokens
   (`i < 0`). This is the primary update path.

3. **File completion** (receiver.c:395-396): `end_progress(total_size)` called
   after the delta token loop finishes.

### Throttling: 1-second minimum between updates

`show_progress()` (progress.c:224) enforces a 1-second minimum between ring
rotations:

```c
if (msdiff(&ph_list[newest_hpos].time, &now) < 1000)
    return;
```

When less than 1 second has passed since the last sample, `show_progress()`
returns without calling `rprint_progress()`. This prevents excessive terminal
output and keeps the sliding window's time resolution at 1 second.

### First call initialization

On the first call (progress.c:205-222), all 5 ring slots are initialized to the
same `(time, ofs)` sample. If recent data from a previous file was received
within 1500ms, the start time inherits from that sample (progress.c:211) -
this avoids a rate spike when a new file begins immediately after the previous
one.

### progress2 mode: overall byte counters

In progress2 mode (`INFO_GTE(PROGRESS, 2)`), `show_progress()` at
progress.c:200-203 transforms the per-file offset into overall transfer counters:

```c
ofs = stats.total_transferred_size - size + ofs;
size = stats.total_size;
```

The sliding window then tracks overall bytes/rate rather than per-file progress.
Similarly, `end_progress()` at progress.c:172-174 uses `stats.total_transferred_size`
and `stats.total_size` as the final values.

### progress2 mode: per-file completion behavior

In progress2 mode, `end_progress(0)` is called at phase boundaries
(receiver.c:565-566) when `NDX_DONE` is received - not after each file. Per-file
completions at receiver.c:395-396 still call `end_progress(total_size)`, but with
`INFO_GTE(PROGRESS, 2)` the function uses overall stats, and the trailing `eol`
is padded without a newline (progress.c:84-91). The visual effect: the single
progress line continuously updates in place until the transfer completes.

### SIGINFO instant progress

On platforms with SIGINFO (macOS) or SIGVTALRM, a signal handler sets
`want_progress_now = True` (main.c:1616-1617) when progress is not already
enabled. The receiver checks this at receiver.c:895 after each file and calls
`instant_progress()` for a one-shot status update.

## 6. The `xfr#` / `to-chk` counter format

### Counter semantics

- `xfr#N`: The N-th file actually transferred in this session
  (`stats.xferred_files`). Incremented only for files that involved data
  transfer, not for skipped or up-to-date files.

- `to-chk=R/T` or `ir-chk=R/T`: Files remaining to check out of total.
  - `R = stats.num_files - current_file_index - 1`
  - `T = stats.num_files`
  - Prefix is `to` when `flist_eof` (file list complete), `ir` during
    incremental recursion (INC_RECURSE)

### `current_file_index` tracking

`set_current_file_index()` (progress.c:147-156) is called from the receiver to
track which file is currently being processed. It handles both sorted and
unsorted file lists, adjusting for sub-list numbering in INC_RECURSE mode.

## 7. oc-rsync current progress infrastructure

### Option parsing

- `crates/cli/src/frontend/execution/flags/info.rs`: `InfoFlagSpec` for
  `progress` with `max_level: 2, strict_cap: true`. Both level 1 (PerFile) and
  level 2 (Overall) are parsed.
- `crates/cli/src/frontend/progress/mode.rs`: `ProgressSetting` enum with
  `Unspecified`, `Disabled`, `PerFile`, `Overall`. Resolves to `ProgressMode`
  (`PerFile` or `Overall`).
- `crates/logging/src/levels/info.rs`: `InfoFlag::Progress` with `progress: u8`
  level field. `config.apply_info_flag("progress2")` correctly sets level 2.

### Format functions

- `format_progress_bytes`: thousands-separated (non-human) or human-readable
  suffixes. Maps to upstream `human_num()`.
- `format_progress_percent`: `N%` or `??%` placeholder. Maps to upstream `pct`.
- `format_progress_rate`: bytes/s -> kB/s/MB/s/GB/s with base-1024 scaling.
  Matches upstream tier logic.
- `format_progress_rate_decimal`: Base-1024 kB/s/MB/s/GB/s matching upstream
  exactly.
- `format_progress_elapsed`: `H:MM:SS` format. Matches upstream `%4u:%02u:%02u`
  structure.

### Sliding-window ETA estimator

`RemainingTimeEstimator` in `crates/cli/src/frontend/progress/format/remaining.rs`:

- 5-slot ring buffer (`HISTORY_SLOTS = 5`) matching upstream
  `PROGRESS_HISTORY_SECS`
- 1-second sample interval matching upstream's `msdiff < 1000` throttle
- `9999 * 3600` second overflow guard matching upstream's `> 9999.0 * 3600.0`
  clamp
- Stall handling: returns `0.0` when bytes_delta is zero, matching upstream's
  `rate ? ... : 0.0` behavior
- Comprehensive tests verifying upstream parity

### Live progress rendering

`LiveProgress` in `crates/cli/src/frontend/progress/live.rs`:

- Implements `ClientProgressObserver` for both `PerFile` and `Overall` modes
- Uses `\r` carriage-return for in-place updates
- Renders `xfr#N, to-chk=R/T` / `ir-chk=R/T` suffix
- Per-file mode: prints filename on new line, then inline progress
- Overall mode: single continuously-updated line
- Uses `RemainingTimeEstimator` for mid-transfer ETA, switches to elapsed on
  final tick

### Batch/post-hoc progress rendering

`emit_progress()` in `crates/cli/src/frontend/progress/render.rs`:

- Renders progress lines from collected `ClientEvent` records (non-live)
- Same field widths as upstream format string

### Progress forwarding infrastructure

`ClientProgressForwarder` in `crates/core/src/client/progress.rs`:

- Wraps `LocalCopyRecordHandler` to convert per-file events into
  `ClientProgressUpdate` objects
- Tracks overall transferred bytes, total bytes, elapsed time
- Handles both final updates (file complete) and intermediate progress
- Local copies only - always sets `flist_eof: true` since file lists are
  enumerated eagerly

## 8. Infrastructure gap analysis

### What exists and works

1. **Option parsing**: `--progress`, `--no-progress`, `-P`, `--info=progress`,
   `--info=progress2`, `--info=progress0` all parsed correctly
2. **Format functions**: Bytes, percent, rate, elapsed/remaining all match
   upstream field widths and scaling
3. **Sliding-window estimator**: Faithful port of upstream's ring buffer algorithm
4. **Live progress rendering**: Both per-file and overall modes implemented
5. **`to-chk`/`ir-chk` prefix**: Correctly switches based on `flist_eof`

### Gaps requiring implementation

1. **progress2 final-tick padding** (progress.c:84-91): oc-rsync's `LiveProgress`
   emits `writeln!` on final tick in both modes. Upstream progress2 strips the
   newline and pads with spaces on the final per-file tick. Only the overall
   transfer completion should emit a newline. The current implementation does
   handle this for the overall mode (`final_tick` guard at live.rs:167), but the
   per-file `end_progress` calls within progress2 mode still need the padding
   behavior.

2. **Rate unit base**: oc-rsync's `format_progress_rate` computes rate as
   `bytes / seconds` (bytes/s), then scales with base-1024 thresholds. Upstream
   computes rate as `bytes * 1000.0 / diff / 1024.0` (kB/s), then scales from
   kB/s. The numeric output matches because both paths produce the same kB/s /
   MB/s / GB/s values, but the oc-rsync code path is organized differently - the
   rate starts in bytes/s and the `format_progress_rate_decimal` function applies
   `/ 1024.0` for kB/s, `/ (1024^2)` for MB/s, `/ (1024^3)` for GB/s. This is
   equivalent but should be verified for edge-case precision parity.

3. **Remote/SSH transfer progress**: `ClientProgressForwarder` currently only
   wraps local copy operations. SSH and daemon transfers need equivalent progress
   forwarding from the receiver's delta token loop. The `from_transfer_event`
   constructor exists but always sets `final_update: true` and
   `overall_total_bytes: None` - intermediate progress ticks from remote
   transfers are not yet wired.

4. **ph_start time inheritance** (progress.c:209-213): When a new file begins
   within 1500ms of the previous file's last data, upstream reuses the previous
   timestamp to avoid rate spikes. oc-rsync's `RemainingTimeEstimator` creates a
   fresh estimator per file (in per-file mode) or reuses a single estimator (in
   overall mode). The per-file fresh-start behavior may cause rate spikes on the
   first tick of small files.

5. **SIGINFO handler**: Not implemented. Upstream exposes instant progress via
   `want_progress_now` on SIGINFO (macOS Ctrl+T) or SIGVTALRM. oc-rsync does not
   register these signal handlers.

6. **Elapsed vs remaining on final tick**: In per-file mode, oc-rsync correctly
   switches to elapsed time on `is_final()` (live.rs:119-120). In overall mode,
   it only switches on `final_tick = remaining == 0 && is_final()` (live.rs:167).
   Upstream switches to elapsed when `is_last` is true regardless of mode
   (progress.c:94-98). This means the per-file end-of-file ticks in progress2
   mode should show the cumulative elapsed time, not the ETA - but upstream uses
   `ph_start` (overall start) for this, so the elapsed time is the overall
   transfer duration so far.

7. **`current_file_index` tracking**: The `to-chk=R/T` counter requires knowing
   which file index is currently being processed. oc-rsync passes this through
   `ClientProgressUpdate::remaining()` and `total()`, derived from the
   `ClientProgressForwarder`'s count of progress events. For local copies this
   works, but for remote transfers the counters need to track file-list indices
   rather than transfer-event counts.

## 9. Summary

The upstream progress format is a single `rprintf` call with 6 fields:
`\r` + 15-char bytes + 4-char percent + 11-char rate + 10-char time + variable
trailer. Rate uses a 5-second sliding window with 1-second sample intervals;
ETA divides remaining bytes by the windowed rate. Updates are triggered per
`recv_token()` call but throttled to one per second by the ring rotation guard.
progress2 mode transforms per-file offsets into overall transfer counters and
suppresses per-file newlines.

oc-rsync has substantial progress infrastructure already in place - format
functions, sliding-window estimator, live rendering for both modes, and option
parsing all match upstream semantics. The primary gaps are in wiring remote
transfer progress updates, SIGINFO support, and minor behavioral details around
final-tick padding in progress2 mode.
