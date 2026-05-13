# Progress line byte-for-byte format audit vs upstream rsync 3.4.1

Tracking issue: #2110. Last verified: 2026-05-13 against `origin/master`.

Sources: `target/interop/upstream-src/rsync-3.4.1/progress.c`;
`crates/cli/src/frontend/progress/live.rs`,
`crates/cli/src/frontend/progress/render.rs`,
`crates/cli/src/frontend/progress/format/progress.rs`,
`crates/cli/src/frontend/progress/format/rate.rs`,
`crates/cli/src/frontend/progress/format/size.rs`.

## 1. Upstream progress format

### 1.1 Core format string

`progress.c:129` - `rprint_progress()`:

```c
rprintf(FCLIENT, "\r%15s %3d%% %7.2f%s %s%s",
    human_num(ofs), pct, rate, units, rembuf, eol);
```

Six fields separated by single spaces (except `rate`+`units` which are
glued together with no space):

1. `\r` - carriage return for single-line refresh.
2. `%15s` - bytes transferred, right-aligned in 15 columns.
3. `%3d%%` - integer percentage, right-aligned in 3 columns, literal `%`.
4. `%7.2f%s` - rate value (7 columns, 2 decimals) glued to unit suffix.
5. `%s` - remaining/elapsed time string.
6. `%s` - end-of-line trailer.

### 1.2 Bytes field via `human_num()`

`human_num()` (`inums.h:33`) delegates to `do_big_num(num, human_readable,
NULL)` in `lib/compat.c:170`. The `human_readable` global defaults to 1
(`options.c:110`), incremented by each `-h` flag (`options.c:1557`).

Behavior by level:

- `human_readable == 0` (`--no-h`): No separators, plain digits.
- `human_readable == 1` (default): Thousands separators inserted every
  3 digits. Separator character is locale-dependent: comma when the
  locale decimal point is `.`, period when it is `,`
  (`lib/compat.c:27-39`).
- `human_readable >= 2` (`-hh`): Values >= 1000 formatted as `%.2f`
  with a unit suffix (`K`/`M`/`G`/`T`/`P`), base-1000. Values < 1000
  fall through to the separator path.
- `human_readable >= 3` (`-hhh`): Same as `>= 2` but base-1024.

### 1.3 Rate computation and units

Rate is computed in KiB/s from the start (`progress.c:96`):

```c
rate = (double)(ofs - ph_start.ofs) * 1000.0 / diff / 1024.0;
```

Then scaled by powers of 1024 (`progress.c:108-116`):

```c
if (rate > 1024*1024) {       // > 1 GiB/s
    rate /= 1024.0 * 1024.0;
    units = "GB/s";
} else if (rate > 1024) {     // > 1 MiB/s
    rate /= 1024.0;
    units = "MB/s";
} else {
    units = "kB/s";
}
```

Key: lowercase `k` in `kB/s` (SI convention). No `B/s` step - even
sub-kB/s rates display as fractional `kB/s`. Division by zero avoided by
clamping `diff = 1` when millisecond delta is zero (`progress.c:95,103`).

### 1.4 Time field (remaining vs elapsed)

Mid-transfer: estimated remaining time computed as seconds until completion.

```c
remain = rate ? (double)(size - ofs) / rate / 1000.0 : 0.0;
```

Final update (`is_last`): total elapsed time in seconds.

```c
remain = (double)diff / 1000.0;
```

Format (`progress.c:121-125`):

```c
snprintf(rembuf, sizeof rembuf, "%4u:%02u:%02u",
    (unsigned int)(remain / 3600.0),
    (unsigned int)(remain / 60.0) % 60,
    (unsigned int)remain % 60);
```

Hours right-aligned in 4 columns. Minutes and seconds zero-padded to 2
digits. Fixed width: 10 characters (`HHHH:MM:SS`).

Overflow guard (`progress.c:118-119`): when `remain < 0` or
`remain > 9999 * 3600`, displays literal `  ??:??:??` (2 leading spaces +
8 chars = 10 chars total).

### 1.5 End-of-line trailer

Final update (`progress.c:78-82`):

```c
snprintf(eol, sizeof eol,
    " (xfr#%d, %s-chk=%d/%d)\n",
    stats.xferred_files, flist_eof ? "to" : "ir",
    stats.num_files - current_file_index - 1,
    stats.num_files);
```

- Leading space before `(`.
- `xfr#N` - 1-based transfer count, bare integer (no commas).
- `to-chk` when file list is complete, `ir-chk` during incremental
  recursion.
- Remaining and total are bare integers.
- Trailing `\n`.

Under `INFO_GTE(PROGRESS, 2)` (overall progress), the `\n` is stripped
and the line is padded with trailing spaces to prevent shortening artifacts
(`progress.c:83-92`).

Mid-transfer: `eol` is `"  "` (two trailing spaces, `progress.c:100`).

### 1.6 Refresh cadence

`show_progress()` returns early if fewer than 1000ms have elapsed since the
last history sample (`progress.c:224`). Rate is averaged over the last 5
seconds (`PROGRESS_HISTORY_SECS = 5`, `progress.c:37`).

## 2. oc-rsync implementation

### 2.1 Live progress path

`crates/cli/src/frontend/progress/live.rs:63-160` implements
`ClientProgressObserver`. The per-file format (`live.rs:88-106`):

```rust
let size_field = format!("{:>15}", format_progress_bytes(bytes, ...));
let percent_field = format!("{percent:>4}");
let rate_field = format!("{:>12}", format_progress_rate(bytes, ...));
let elapsed_field = format!("{:>11}", format_progress_elapsed(...));
write!(writer,
    "{size_field} {percent_field} {rate_field} {elapsed_field} (xfr#{xfr_index}, to-chk={remaining}/{total})"
)?;
```

Carriage return: emitted before the line when `line_active` is true
(`live.rs:99-101`).

### 2.2 Batch progress path

`crates/cli/src/frontend/progress/render.rs:156-197` emits completed
transfer progress with newlines (no `\r` refresh):

```rust
writeln!(stdout, "{}", event.relative_path().display())?;
writeln!(stdout,
    "{size_field} {percent_field} {rate_field} {elapsed_field} (xfr#{xfr_index}, to-chk={remaining}/{total})"
)?;
```

### 2.3 Bytes formatting

`format_decimal_bytes()` (`format/size.rs:27-39`): Always uses commas as
thousands separators regardless of locale. Right-aligned to 15 chars.

`format_human_bytes()` (`format/size.rs:41-63`): SI units (base-1000)
with `K`/`M`/`G`/`T`/`P` suffixes, `{:.2}` precision. Values < 1000
returned as plain digits.

### 2.4 Rate formatting

`format_progress_rate_decimal()` (`format/rate.rs:136-148`): Uses
binary thresholds (1024-based: `KIB`, `MIB`, `GIB`), matching upstream.
Unit strings: `kB/s`, `MB/s`, `GB/s`. Precision: `{:.2}` (variable width).

Zero/stall case (`format/rate.rs:105-111`): Returns `"0.00kB/s"` in
default mode, `"0.00B/s"` in human-readable mode.

`format_progress_rate_human()` (`format/rate.rs:150-153`): Delegates to
`format_verbose_rate_human()`, which uses base-1000 SI units. This
differs from the default path (base-1024).

### 2.5 Percent formatting

`format_progress_percent()` (`format/progress.rs:7-17`): Integer division
(truncation), matching upstream's `(int)` cast. Returns `"??%"` when total
is unknown. Returns `"100%"` when total is zero.

### 2.6 Time formatting

`format_progress_elapsed()` (`format/progress.rs:20-26`): Always formats
elapsed time (never remaining time). Hours have no minimum width. Result
right-aligned to 11 chars in the caller.

```rust
format!("{hours}:{minutes:02}:{seconds:02}")
```

No overflow guard - extreme durations produce arbitrarily wide output.

## 3. Field-by-field comparison

### 3.1 Bytes transferred

| Aspect | Upstream | oc-rsync | Match |
|--------|----------|----------|-------|
| Field width | 15 chars (`%15s`) | 15 chars (`{:>15}`) | YES |
| Alignment | Right | Right | YES |
| Default separator | Locale-dependent (`,` or `.`) | Always `,` | PARTIAL |
| `-hh` suffixes | `K`/`M`/`G`/`T`/`P` | `K`/`M`/`G`/`T`/`P` | YES |
| `-hh` precision | `%.2f` | `{:.2}` | YES |
| `-hh` base | 1000 | 1000 | YES |
| `-hhh` base | 1024 | Not supported | NO |
| `--no-h` | No separators | Not supported | NO |

### 3.2 Percentage

| Aspect | Upstream | oc-rsync | Match |
|--------|----------|----------|-------|
| Width spec | `%3d%%` (3 digits + `%`) | `{:>4}` (4 total) | YES |
| Rounding | Truncation (`(int)` cast) | Truncation (integer div) | YES |
| 100% at completion | Explicit `ofs == size` check | When total == 0 | PARTIAL |
| Unknown total | Not applicable | `??%` | N/A |

### 3.3 Transfer rate

| Aspect | Upstream | oc-rsync | Match |
|--------|----------|----------|-------|
| Base | 1024 (binary) | 1024 (binary) | YES |
| Unit strings | `kB/s`, `MB/s`, `GB/s` | `kB/s`, `MB/s`, `GB/s` | YES |
| Precision | 2 decimal places | 2 decimal places | YES |
| Value width | Fixed 7 chars (`%7.2f`) | Variable (`{:.2}`) | NO |
| Combined width | 11 chars (`%7.2f` + 4) | 12 chars (`{:>12}`) | NO |
| kB->MB threshold | `rate > 1024` (kB/s units) | `rate >= MIB` (bytes/s) | YES |
| MB->GB threshold | `rate > 1024*1024` (kB/s units) | `rate >= GIB` (bytes/s) | YES |
| Zero fallback | `0.00kB/s` (clamped diff=1) | `0.00kB/s` | YES |

### 3.4 Time field

| Aspect | Upstream | oc-rsync | Match |
|--------|----------|----------|-------|
| Format | `HHHH:MM:SS` | `H:MM:SS` | PARTIAL |
| Hours width | 4 chars (`%4u`) | Variable (no min) | NO |
| Total width | 10 chars fixed | 11 chars right-aligned | NO |
| Semantics: mid-file | Estimated remaining time | Elapsed time | NO |
| Semantics: final | Total elapsed time | Elapsed time | YES |
| Overflow guard | `  ??:??:??` | None | NO |
| Rate averaging | 5-second sliding window | Per-file total | NO |

### 3.5 Trailer

| Aspect | Upstream | oc-rsync | Match |
|--------|----------|----------|-------|
| `xfr#N` format | `xfr#%d` (bare integer) | `xfr#{index}` | YES |
| Check prefix | `to-chk` or `ir-chk` | Always `to-chk` | NO |
| Counts format | Bare integers | Bare integers | YES |
| Leading space | ` (xfr#...)` | `(xfr#...)` (no leading space) | NO |
| Mid-file eol | `"  "` (two spaces) | Not emitted (live: line stays) | PARTIAL |

### 3.6 Line framing

| Aspect | Upstream | oc-rsync | Match |
|--------|----------|----------|-------|
| Leading `\r` | Every update | Only when `line_active` (live) | YES |
| Trailing `\n` (final) | In eol string | After final update | YES |
| Space-padding (P2) | Pads to `last_len` | Not implemented | NO |
| Refresh interval | 1000ms minimum | Per-event | NO |

## 4. Summary of divergences

| ID | Field | Severity | Description |
|----|-------|----------|-------------|
| D1 | Bytes | Low | Thousands separator always comma, not locale-aware |
| D2 | Bytes | Low | `-hhh` (base-1024) and `--no-h` modes not supported |
| D3 | Rate | Medium | Value width not fixed to 7 chars (`%7.2f` vs `{:.2}`) |
| D4 | Rate | Low | Combined field 12 chars vs upstream's 11 chars |
| D5 | Rate | Low | Human-readable mode uses base-1000 vs upstream base-1024 |
| D6 | Time | High | Mid-file shows elapsed time, not estimated remaining |
| D7 | Time | Medium | No 5-second sliding-window rate averaging |
| D8 | Time | Low | Hours not fixed to 4 chars (`%4u` vs variable) |
| D9 | Time | Low | No overflow guard (`  ??:??:??` sentinel missing) |
| D10 | Trailer | Low | `ir-chk` never emitted during incremental recursion |
| D11 | Trailer | Low | Missing leading space before `(xfr#...)` |
| D12 | Framing | Low | No trailing space-padding for shrinking lines |
| D13 | Framing | Low | No 1000ms throttle on refresh interval |

## 5. Recommended fixes

### P0 - Must fix for behavioral parity

**D6 - Remaining time vs elapsed time**: Most significant divergence. Users
expect the time field to show estimated remaining time during transfer,
switching to total elapsed time only on the final update. Requires:

- Computing transfer rate from recent history (sliding window matching
  upstream's `PROGRESS_HISTORY_SECS = 5`).
- Estimating remaining time as `(total_size - transferred) / rate`.
- Switching to elapsed time on the final update.
- This also resolves D7 (rate averaging).

### P1 - Should fix for pixel-perfect output

**D3 - Rate value width**: Use `format!("{:>7.2}", rate)` for a fixed
7-character numeric value, matching `%7.2f`. This makes the rate column
start at a consistent position and also resolves D4.

**D8 - Hours width**: Use `format!("{hours:>4}:{minutes:02}:{seconds:02}")`
for a fixed 10-character time field, matching `%4u:%02u:%02u`.

**D9 - Overflow guard**: When remaining time is negative or exceeds
`9999 * 3600` seconds, display `  ??:??:??` (10 chars).

**D11 - Leading space**: Add a space before `(xfr#...)` in both live and
batch paths.

### P2 - Nice to have

**D1 - Locale-aware separator**: Detect the locale decimal point and choose
the correct thousands separator, matching `lib/compat.c:27-39`.

**D2 - `--no-h` and `-hhh` modes**: Thread the full `human_readable` level
(0/1/2/3) through the formatting stack.

**D5 - Rate base consistency**: Progress rate in human-readable mode should
use base-1024 (matching the default path and upstream), not base-1000 SI.

**D10 - `ir-chk` prefix**: Emit `ir-chk` when incremental recursion is
active and `flist_eof` is false.

**D12 - Line padding**: Track maximum line length; pad shorter lines with
trailing spaces to prevent terminal artifacts.

**D13 - Refresh throttle**: Rate-limit live progress updates to at most
once per second, matching upstream's 1000ms guard in `show_progress()`.

## 6. Test plan

Add golden-byte fixtures under `tests/golden/progress/`:

- `per_file_mid.txt` - mid-transfer line at 50% of 100 MiB:
  `\r      52,428,800  50%   12.34MB/s    0:00:04  `
  (leading `\r`, trailing two spaces, no `\n`).
- `per_file_final.txt` - final line:
  `\r     104,857,600 100%   12.34MB/s    0:00:08 (xfr#1, to-chk=0/1)\n`.
- `ir_chk.txt` - incremental recursion line:
  `... (xfr#3, ir-chk=42/118)\n`.
- `stalled_eta.txt` - stalled transfer:
  `...   ??:??:??  `.
- `progress2_pad.txt` - `--info=progress2` with trailing space-pad, no `\n`.

Validation script to capture real upstream output preserving `\r`:

```sh
dd if=/dev/urandom of=/tmp/p100m bs=1M count=100
script -q /dev/null rsync    --progress /tmp/p100m /tmp/up/ > /tmp/up.raw 2>&1
script -q /dev/null oc-rsync --progress /tmp/p100m /tmp/oc/ > /tmp/oc.raw 2>&1
diff <(xxd /tmp/up.raw) <(xxd /tmp/oc.raw)
```

## 7. Upstream source references

- `progress.c:69-135` - `rprint_progress()` - main format function.
- `progress.c:37` - `PROGRESS_HISTORY_SECS = 5` - sliding window size.
- `progress.c:96,104` - Rate computation in KiB/s.
- `progress.c:108-116` - Unit thresholds (1024-based).
- `progress.c:118-125` - Time formatting and overflow guard.
- `progress.c:129-130` - Final `rprintf` format string.
- `progress.c:167-180` - `end_progress()` - final update dispatch.
- `progress.c:182-241` - `show_progress()` - mid-file update with history.
- `lib/compat.c:170-246` - `do_big_num()` - locale-aware number formatting.
- `lib/compat.c:27-39` - `get_number_separator()` - locale detection.
- `inums.h:33-37` - `human_num()` inline wrapper.
- `main.c:416-465` - `output_summary()` - stats and totals format.
- `options.c:110` - `human_readable = 1` (default).

## 8. oc-rsync source references

- `crates/cli/src/frontend/progress/live.rs:63-160` - live progress
  observer.
- `crates/cli/src/frontend/progress/render.rs:156-197` - batch progress
  emit.
- `crates/cli/src/frontend/progress/format/progress.rs:7-17` - percent
  formatter.
- `crates/cli/src/frontend/progress/format/progress.rs:20-26` - elapsed
  time formatter.
- `crates/cli/src/frontend/progress/format/rate.rs:100-167` - rate
  formatting.
- `crates/cli/src/frontend/progress/format/size.rs:9-63` - byte count
  formatting.
