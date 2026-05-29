# Checksum-mode statx/syscall overhead analysis (STX-1, STX-4)

## Observed gap

strace on a `--checksum` transfer of a 500-file corpus shows oc-rsync
issuing 6,691 statx calls vs upstream rsync's 2,006 - a 3.34x ratio.
The gap is specific to `--checksum` mode; quick-check (mtime+size) mode
does not exhibit it because the receiver skips whole-file hashing.

## Root causes

### 1. BufReader EOF probe (STX-6)

The parallel file hasher in `crates/checksums/src/parallel/files.rs`
wraps the file in `BufReader` and calls `read_to_end()` for files below
`max_memory_file_size`. `read_to_end()` internally issues an extra
`read()` that returns 0 to detect EOF. For the streaming path (files
above the threshold), the loop calls `reader.read(&mut buffer)` until it
returns 0 - the same extra-read-for-EOF pattern.

The receiver's `file_checksum_matches()` in
`crates/transfer/src/receiver/quick_check.rs` already avoids this: it
uses the known `file_size` from the flist to loop with `read_exact()`
and a decrementing `remaining` counter, issuing exactly
`ceil(file_size / 64K)` reads per file - no EOF probe.

The parallel hasher should adopt the same sized-read pattern. The file
size is already available from `file.metadata()` at the top of
`hash_file_internal()`.

### 2. Redundant stat calls (STX-8)

During `--checksum` mode, oc-rsync re-stats files whose metadata (size,
mtime) is already cached in the flist. Each call to `File::open()` +
`file.metadata()` in the checksum path issues an `openat` + `fstat`
pair. On the receiver side the flist already carries the authoritative
`st_size`, so the extra `fstat` is redundant. On the sender side the
flist-building pass already stats every file; the checksum pass re-opens
and re-stats to get the file size for hashing.

Eliminating these redundant stats requires threading the cached flist
size into the checksum call sites so they can use sized reads without
querying the kernel again.

## Upstream read pattern (STX-4)

Upstream `file_checksum()` in `checksum.c:402` reads files through
`map_file()` / `map_ptr()` - a windowed-read abstraction, not mmap.

Key characteristics:

- **Known file size.** `file_checksum()` receives `const STRUCT_STAT *st_p`
  from the caller. The file size `st_p->st_size` drives the read loop.
  No stat inside the function.
- **No EOF probe.** The loop `for (i = 0; i + CHUNK_SIZE <= len; i += CHUNK_SIZE)`
  uses the known length. The remainder `len - i` handles the final
  partial chunk. No read-until-zero pattern.
- **CHUNK_SIZE = 32 KB.** Defined in `rsync.h:158`. Each `map_ptr()`
  call triggers a `read()` of up to `MAX_MAP_SIZE` (256 KB, `rsync.h:159`)
  into a reusable window buffer, so the actual kernel reads are 256 KB
  each. Subsequent `map_ptr()` calls for adjacent offsets hit the
  in-window cache without a syscall.
- **Single fd, single open.** `do_open_checklinks()` opens the file
  once. `close(fd)` and `unmap_file(buf)` clean up. One `open` + one
  `fstat` (from the caller's prior `lstat`) per file.

### Comparison with oc-rsync

| Aspect | Upstream rsync | oc-rsync (current) |
|---|---|---|
| File size source | Caller-provided `st_p->st_size` | `file.metadata().len()` - extra fstat |
| Read loop termination | Known length, no EOF probe | `read_to_end()` or `read()` until 0 |
| Buffer strategy | 256 KB sliding window (`map_ptr`) | 64 KB `BufReader` or mmap |
| Syscalls per file | 1 open + ceil(size/256K) reads | 1 open + 1 fstat + ceil(size/64K) reads + 1 EOF read |

## Fix references

- **STX-6:** Replace BufReader/read_to_end with pre-sized read loop
  using known file size, matching upstream's sized-loop pattern.
- **STX-8:** Thread cached flist metadata into checksum call sites to
  eliminate redundant stat calls.
