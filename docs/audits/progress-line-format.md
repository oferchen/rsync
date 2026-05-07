# Progress line byte-for-byte format audit vs upstream

Tracking issue: #2110. Last verified: 2026-05-07 against `origin/master`.

Sources: `target/interop/upstream-src/rsync-3.4.1/progress.c`;
`crates/cli/src/frontend/progress_format.rs`,
`progress/format/progress.rs`, `crates/core/src/client/progress.rs`.

## 1. Upstream progress format

Format string at `progress.c:129`:

```c
rprintf(FCLIENT, "\r%15s %3d%% %7.2f%s %s%s",
    human_num(ofs), pct, rate, units, rembuf, eol);
```

Field widths (left to right):

- Leading `\r`. Single-line refresh; no newline until `is_last`.
- `%15s` bytes via `human_num()` (`util2.c`), comma thousands
  separators, right-aligned in 15 cols.
- One space.
- `%3d%%` percent. Integer 0-100, 3 cols, `%` suffix.
- One space.
- `%7.2f` rate, two decimals, glued to units suffix
  (`kB/s`/`MB/s`/`GB/s`) chosen at `progress.c:108-116` with
  `rate > 1024*1024` -> GB/s, `rate > 1024` -> MB/s, else `kB/s`.
  Lowercase-k SI spelling. No `B/s` step. No `inf`: when divisor
  is 0, `msdiff` clamps `diff = 1` (`progress.c:95,103`).
- One space.
- `rembuf` ETA, `%4u:%02u:%02u` -> `HHHH:MM:SS`, hours field
  4 cols (`progress.c:121`). When `remain < 0` or
  `remain > 9999*3600` it prints literal `  ??:??:??` with two
  leading spaces (`progress.c:118-119`). No D:HH:MM:SS form.
- `eol`. Mid-transfer two spaces (`progress.c:100`). Final line
  ` (xfr#%d, %s-chk=%d/%d)\n` with `%s` = `to` once `flist_eof`
  is true, else `ir` (`progress.c:80`). Under
  `INFO_GTE(PROGRESS, 2)` the `\n` is replaced by space-padding
  to `last_len` (`progress.c:83-92`). Counts are bare integers.

Refresh cadence: `show_progress` returns early if less than
1000ms have elapsed since the last sample (`progress.c:224`).

## 2. oc-rsync implementation

`PerFileProgress::format_line` (`progress_format.rs:103-111`):

```rust
format!("{bytes_str:>15} {percent:>4}   {rate_str:>12}    {elapsed_str}")
```

`OverallProgress::format_line` (`progress_format.rs:202-219`):

```rust
format!("{files_done:>6}/{} files  {percent} (xfr#{xfr}, to-chk={to_chk}/{})", ...)
```

`format_rate` (`progress_format.rs:343-356`) picks suffix on
`>= 1024*1024*1024`, `>= 1024*1024`, else `kB/s`. `format_number`
(`progress_format.rs:424-437`) inserts commas every three digits.
`format_progress_percent` (`progress/format/progress.rs:7-17`)
emits `??%` when total bytes are unknown.
`ClientProgressForwarder::handle_progress`
(`client/progress.rs:247-285`) feeds observers with no
line-rewrite framing; nothing emits `\r`.

## 3. Diff method (byte-for-byte)

Reproducer on a Linux host with both binaries:

```sh
dd if=/dev/urandom of=/tmp/p100m bs=1M count=100
script -q /dev/null rsync    -P /tmp/p100m /tmp/up/ 2> /tmp/up.stderr
script -q /dev/null oc-rsync -P /tmp/p100m /tmp/oc/ 2> /tmp/oc.stderr
diff <(xxd /tmp/up.stderr) <(xxd /tmp/oc.stderr)
```

`script -q /dev/null` preserves the `\r` framing that bare
shell redirection swallows. Diffs from source inspection:

| Field | Upstream | oc-rsync |
|-------|----------|----------|
| Refresh | `\r` per sample | newline per update |
| Percent col | `%3d%%` (3 cols) | `{:>4}` (4 cols) |
| Rate col | `%7.2f<units>` | `{:>12}` rate string |
| ETA hours | `%4u` (4 cols) | bare `{hours}` |
| Mid-line tail | two spaces | empty |
| Final tail | `ir-chk=` until `flist_eof` | always `to-chk=` |
| Unknown ETA | `  ??:??:??` | `0:00:00` |

## 4. Known gaps

- No `\r` single-line refresh; `progress/live.rs` prints a fresh
  line per update.
- Hours unpadded; upstream uses 4 cols.
- Rate column uses `{:>12}` rather than `%7.2f<units>`.
- `ir-chk=` prefix never emitted; upstream selects `ir`/`to`
  from `flist_eof` (`progress.c:80`).
- `  ??:??:??` stalled-ETA sentinel missing. `format_eta`
  returns `0:00:00` (`progress_format.rs:370-372`).
- D:HH:MM:SS branch (`progress_format.rs:380-382`): no upstream counterpart.
- xfr/chk counts use commas; upstream emits bare integers.
- `INFO_GTE(PROGRESS, 2)` space-pad to `last_len` not done.
- ASCII bar: neither side renders one (no gap).
- `inf` rate: unused upstream; oc-rsync's zero fallback is
  acceptable provided parity with the clamped sample is noted.

## 5. Test plan

Add golden fixtures under `tests/golden/` (new directory):

- `progress_per_file_50pct.txt` 100MB at 50%, mid-transfer:
  `\r      52,428,800  50%   12.34MB/s    0:00:04  ` (leading
  `\r`, trailing two spaces, no `\n`).
- `progress_per_file_final.txt` final line:
  `\r     104,857,600 100%   12.34MB/s    0:00:08 (xfr#1, to-chk=0/1)\n`.
- `progress_ir_chk.txt` intermediate recursion line ending
  `... (xfr#3, ir-chk=42/118)\n`.
- `progress_unknown_eta.txt` stalled-rate line ending
  `   ??:??:??  ` (two leading spaces inside `rembuf`).
- `progress_progress2_pad.txt` `--info=progress2` line with
  trailing space-pad, no `\n`.

Harness: extend `crates/cli/src/frontend/tests/progress_render.rs`
to load each fixture, drive the formatter with the matching
scenario, and compare via `assert_eq!`. Capture upstream once
with `script -q /dev/null rsync -P ...` to preserve `\r`, then
check the file into `tests/golden/`.
