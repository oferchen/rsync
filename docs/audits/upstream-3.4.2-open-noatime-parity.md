# O_NOATIME propagation parity vs rsync 3.4.2

Tracking task: #2236 (parent #2215). Last verified 2026-05-15 against
origin/master.

## 1. Upstream change

Rsync 3.4.2 broadens `--open-noatime` so the flag is honoured by the
`do_open_nofollow()` path as well as `do_open()`. In 3.4.1, only
`do_open()` OR'd `O_NOATIME` into the open flags; sender source-file reads
that go through `do_open_checklinks()` -> `do_open_nofollow()` (the
default branch when `--copy-links` / `--copy-unsafe-links` are off) did
not get `O_NOATIME` at all and silently bumped the source `atime`.

Upstream `syscall.c` after the fix:

```c
// upstream: syscall.c do_open / generator.c read_sum_head
int do_open(const char *pathname, int flags, mode_t mode) {       // line 220
#ifdef O_NOATIME
    if (open_noatime) flags |= O_NOATIME;                         // line 228
#endif
    return open(pathname, flags | O_BINARY, mode);
}

int do_open_nofollow(const char *pathname, int flags) {           // line 669
#ifdef O_NOATIME
    if (open_noatime) flags |= O_NOATIME;                         // line 687 (NEW in 3.4.2)
#endif
#ifdef O_NOFOLLOW
    return open(pathname, flags|O_NOFOLLOW);
#else
    /* ... lstat/fstat race-safe fallback ... */
#endif
}
```

Both paths feed `do_open_checklinks()` (`syscall.c:809`), which the
sender uses for every source file (`sender.c:355`).

## 2. oc-rsync source-file open audit

### 2.1 Sender (transfer crate, server-side generator)

| Site | Reads | Verdict before fix |
|------|-------|--------------------|
| `crates/transfer/src/generator/mod.rs:738` (`open_source_reader`) | source file body for delta / whole-file send | **GAP**: plain `File::open`, no `O_NOATIME` |
| `crates/transfer/src/generator/mod.rs:726` (`reader_from_path_with_depth`) | source file body via io_uring on large files | **GAP**: io_uring `IoUringReader::open` and `StdFileReader::open` do not request `O_NOATIME` |
| `crates/transfer/src/generator/delta.rs:314` | source re-read for whole-file checksum after `Copy` tokens | **GAP**: plain `File::open` |
| `crates/transfer/src/generator/delta.rs:418` | source re-read for CPRES_ZLIB dictionary sync | **GAP**: plain `File::open` |

All four sites mirror upstream's `do_open_checklinks()` call inside the
sender / generator and must propagate `--open-noatime` to be on parity
with 3.4.2.

### 2.2 Local-copy executor

| Site | Status |
|------|--------|
| `crates/engine/src/local_copy/executor/file/copy/transfer/open.rs:48` | **OK**: `open_source_file()` already wires `O_NOATIME` via `OpenOptionsExt::custom_flags(libc::O_NOATIME)` on Linux/Android with `EPERM/EACCES/EINVAL/ENOTSUP/EROFS` fallback to a regular open. Configured by `LocalCopyOptions::open_noatime` (set in `crates/cli/.../workflow/run.rs:373-380` and threaded through `LocalCopyContext::open_noatime_enabled`). |

The local-copy path is already parity-correct and serves as the
reference implementation for the sender fix.

### 2.3 Receiver-side basis / destination reads

`do_open` (regular open) is the only upstream path used for these reads.
oc-rsync sites:

| Site | Reads |
|------|-------|
| `crates/transfer/src/receiver/basis.rs:119,134` | basis-dir lookup for `--compare-dest` / `--copy-dest` / `--link-dest` |
| `crates/transfer/src/receiver/quick_check.rs:232` | `--checksum` whole-file hash of destination |
| `crates/transfer/src/transfer_ops/streaming.rs:123` and `response.rs:123` | basis `MapFile::open(...)` for delta apply |
| `crates/transfer/src/map_file/buffered.rs:51` | basis mmap fallback |
| `crates/transfer/src/pipeline/async_signature.rs:184` | basis file open for signature generation |

These are destination-side reads of files that will be overwritten or
renamed-over; upstream applies `O_NOATIME` here too when set, but the
atime of a destination file about to be replaced is not user-visible.
**Verdict: low-risk gap**, deferred behind the sender fix.

### 2.4 Metadata / unrelated opens

| Site | Reads |
|------|-------|
| `crates/transfer/src/generator/filters.rs:319` | `--files-from` list file (control input, not transferred) |
| `crates/transfer/src/disk_commit/process.rs:232,236,344` | destination file `O_WRONLY` (write side, not a source read) |
| `crates/transfer/src/temp_guard.rs:130,147` | temp file for atomic rename (write side) |

Out of scope: not source-file reads.

## 3. Remediation in this branch

The sender gap (section 2.1) is the user-visible regression. Fix:

1. Add `open_noatime: bool` to `transfer::config::WriteConfig` (default
   `false`) plus a `TransferConfigBuilder::open_noatime(...)` setter and
   wire it from `ClientConfig::open_noatime()` in
   `crates/core/src/client/remote/daemon_transfer/orchestration/server_config.rs`.
2. Extend `crates/transfer/src/generator/mod.rs::open_source_reader` and
   the two re-read paths in `crates/transfer/src/generator/delta.rs` to
   request `O_NOATIME` via `OpenOptionsExt::custom_flags(libc::O_NOATIME)`
   on Linux/Android, with the same `EPERM/EACCES/EINVAL/ENOTSUP/EROFS`
   fallback as the local-copy implementation. On other targets the
   helper is a no-op `File::open(path)`.
3. Regression test: a Linux-only test in
   `crates/transfer/src/generator/tests.rs` that creates a source file,
   backdates its `atime`, invokes `open_source_reader` with
   `open_noatime = true`, reads the contents, and verifies that the
   on-disk `atime` is unchanged. Skip on non-Linux and on filesystems
   that reject `O_NOATIME` (mirror the `EPERM/EACCES` fallback).

The io_uring helper (`fast_io::reader_from_path_with_depth`) keeps its
existing signature: the generator falls back to the explicit
`OpenOptions::custom_flags(O_NOATIME)` path when `--open-noatime` is set,
since `IoUringReader::open` does not accept custom open flags and the
extra atime write is what users opt in against.

Receiver-side basis reads (section 2.3) are deferred to a follow-up
because their atime is overwritten on commit; this audit records them so
the next pass can land a single helper across both crates.

## 4. Verdict summary

| Path | Verdict |
|------|---------|
| Sender source-file open (regular + delta re-read + zlib dict sync) | **FIXED** in this branch |
| Local-copy source-file open | OK (already parity-correct) |
| Receiver basis / destination reads | LOW-RISK GAP, follow-up |
| `--files-from` / temp / destination write opens | Out of scope |

`O_NOATIME` is Linux/Android only; all new code is gated with
`#[cfg(any(target_os = "linux", target_os = "android"))]` and falls
through to a plain `File::open` on every other target, matching the
upstream `#ifdef O_NOATIME` guard.
