# Y2038 syscall parity vs rsync 3.4.2

Tracking issue: #2229. Last verified 2026-05-14 against origin/master.

## 1. Upstream fix

Upstream commit replaces a Windows-only macro in `syscall.c`
`do_SetFileTime()` (Cygwin build, `SUPPORT_CRTIMES`):

- Before (`rsync-3.4.1/syscall.c:483`):

  ```c
  int64 temp_time = Int32x32To64(crtime, 10000000) + 116444736000000000LL;
  ```

- After (`rsync-3.4.2/syscall.c:483`):

  ```c
  int64 temp_time = (crtime * 10000000LL) + 116444736000000000LL;
  ```

`Int32x32To64(a, b)` is the legacy Win32 macro `((int64)((int32)(a)) *
(int32)(b))`. It casts both operands to `int32` before the multiply, so a
`time_t crtime >= 2^31` (after 2038-01-19 03:14:07 UTC) is truncated to its
low 32 bits and the resulting NTFS FILETIME silently wraps. The fix lets
the C compiler perform a native `time_t * int64_t` multiplication, which
on every supported toolchain widens to 64 bits and preserves the full
range.

The bug exists only on Cygwin (`#if defined __CYGWIN__`) and only when
`SUPPORT_CRTIMES` is compiled in. No other timestamp path in upstream uses
`Int32x32To64`; `mtime`/`atime` go through `utimensat`/`SetFileTime` via
distinct code paths that already take 64-bit arguments.

## 2. oc-rsync surface area

### 2.1 Wire-protocol time fields

| Field | Type | Site |
|-------|------|------|
| `FileEntry::mtime` | `i64` | `crates/protocol/src/flist/entry/accessors.rs:173` |
| `FileEntry::mtime_nsec` | `u32` | `crates/protocol/src/flist/entry/accessors.rs:180` |
| `FileEntry::atime` | `i64` | `crates/protocol/src/flist/entry/accessors.rs:314` |
| `FileEntry::crtime` | `i64` | `crates/protocol/src/flist/entry/accessors.rs:338` |
| `encode_mtime(mtime: i64, ...)` | `i64` -> `varlong(4)` | `crates/protocol/src/wire/file_entry/encode.rs:255` |
| `encode_atime(atime: i64)` | `i64` -> `varlong(4)` | `crates/protocol/src/wire/file_entry/encode.rs:279` |
| `ModernCodec::read_mtime` | `read_varlong -> i64` | `crates/protocol/src/codec/protocol/modern.rs:57` |
| `LegacyCodec::read_mtime` | `read_uint -> i64::from(u32)` | `crates/protocol/src/codec/protocol/legacy.rs:69` |

Every internal representation is `i64`; varlong on the wire carries up to
8 bytes for protocol >= 30. Verdict: **SAFE**.

### 2.2 Legacy protocol < 30 wire mtime

`encode_mtime` truncates with `mtime as i32` when
`protocol_version < 30` (`crates/protocol/src/wire/file_entry/encode.rs:259`)
and `LegacyCodec::write_mtime` casts via `mtime as u32`
(`crates/protocol/src/codec/protocol/legacy.rs:66`). This mirrors upstream
`flist.c:585` (`write_int(f, modtime)`) and `flist.c` legacy
`write_uint(f, modtime)`. Protocol < 30 corresponds to rsync < 3.0.0 and
is Y2038-broken in upstream by design; oc-rsync matches the upstream wire
contract byte-for-byte. Verdict: **DIVERGENT-BY-DESIGN** (intentional
wire-format parity with a legacy peer; not a regression we introduce).

### 2.3 NTFS FILETIME conversion (the upstream bug site)

oc-rsync has **no equivalent** to upstream `do_SetFileTime()`. We never
build a Windows FILETIME by hand. Timestamp application goes through:

- `filetime` crate 0.2.28 (`crates/metadata/src/apply/timestamps.rs:9`),
  whose `FileTime` carries `seconds: i64` and `nanos: u32` and on Windows
  calls `SetFileTime` after a 64-bit `i64 * 10_000_000 + 116444736000000000`
  multiplication done in safe Rust with full `i128` headroom.
- `rustix::fs::futimens` for fd-based application
  (`crates/metadata/src/apply/timestamps.rs:82`), which takes a
  `Timespec { tv_sec: i64, tv_nsec: i64 }`.
- `libc::setattrlist` on macOS for crtime
  (`crates/metadata/src/apply/timestamps.rs:253`), which takes a
  `libc::timespec` whose `tv_sec: time_t` is `i64` on all macOS targets we
  support.

No `Int32x32To64`-equivalent multiplication exists. There is no `as i32`
cast on a `secs` value in any timestamp application path. Verdict:
**SAFE**.

### 2.4 Source-side stat ingest

| Site | Behaviour |
|------|-----------|
| `crates/flist/src/batched_stat/types.rs:58` | `mtime_sec: stat_buf.st_mtime` (-> `i64`), `mtime_nsec: st_mtime_nsec as u32` |
| `crates/metadata/src/apply/timestamps.rs:189` | `created.duration_since(UNIX_EPOCH)?.as_secs() as i64` (`as_secs() -> u64`, widened) |

`st_mtime` is `time_t` (64-bit on every supported tier-1 target except
i686-glibc, where rsync's own behaviour matches us). The nsec cast is
bounded by `[0, 1_000_000_000)` and is unaffected by Y2038. Verdict:
**SAFE**.

### 2.5 Non-timestamp `time -> i32` casts

| Site | Purpose |
|------|---------|
| `crates/transfer/src/setup/negotiator.rs:194` | Checksum seed: `SystemTime::now().duration_since(UNIX_EPOCH).as_secs() as i32`, XORed with `pid << 6`. Mirrors upstream `options.c:835` (`(int32)time(NULL)`). The seed is intentionally a 32-bit nonce, not a timestamp semantic; truncation is required for byte-for-byte parity with the upstream wire. Verdict: **DIVERGENT-BY-DESIGN**. |
| `crates/transfer/src/lib.rs:551` | `MSG_IO_TIMEOUT` payload: `timeout_secs as i32`. Bounded by user-supplied `--timeout` (max `i32::MAX` seconds = 68 years). Not a wall-clock value. Verdict: **SAFE**. |

## 3. Verdict summary

| Path | Verdict |
|------|---------|
| FileEntry time accessors (mtime/atime/crtime) | SAFE |
| Modern protocol (>=30) wire codec | SAFE |
| Legacy protocol (<30) wire codec | DIVERGENT-BY-DESIGN (upstream parity) |
| NTFS FILETIME conversion (upstream bug site) | SAFE (no equivalent code) |
| Source-side `st_mtime`/`crtime` ingest | SAFE |
| Checksum seed and `MSG_IO_TIMEOUT` | SAFE / DIVERGENT-BY-DESIGN |

oc-rsync is **not affected by the 3.4.2 `Int32x32To64` Y2038 bug**. No
remediation required. We carry no Windows FILETIME hand-conversion of our
own; all timestamp arithmetic uses `i64`/`u64`/`i128` widening or routes
through the `filetime` and `rustix` crates which already perform 64-bit
math.

## 4. Future-proofing notes

- Keep `FileEntry::{mtime, atime, crtime}` at `i64`. Reject any future
  refactor that narrows these to `i32`.
- If we ever add a hand-rolled `SetFileTime` path (e.g. for direct
  `windows-rs` use to bypass `filetime`), the multiplication must be
  `(secs as i64) * 10_000_000i64 + 116_444_736_000_000_000i64`, never
  `(secs as i32) * 10_000_000`.
- The checksum-seed and `MSG_IO_TIMEOUT` casts are deliberately narrow;
  any change must keep wire-format parity with upstream's `int32` field.
