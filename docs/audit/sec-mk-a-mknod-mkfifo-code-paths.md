# mknod/mkfifo code-path inventory for *at sandbox migration (SEC-MK.a)

Scope: every code path in the receiver, generator, and local-copy executor
that creates device nodes (`mknod`) or FIFOs (`mkfifo`). Comment-only
references (doc strings, upstream citations) are excluded - only call sites
that issue a syscall or delegate to one are inventoried. Test-only call
sites are listed separately in a final section.

## Background

The SEC-1 series migrated most path-based syscalls to `*at` variants
(`fstatat`, `unlinkat`, `mkdirat`, `symlinkat`, `linkat`, `renameat`,
`fchmodat`, `fchownat`, `utimensat`) for TOCTOU protection under
`use_chroot=false`. The `DirSandbox` carrier
(`crates/fast_io/src/dir_sandbox/`) holds an open `OwnedFd` for the
destination root and exposes `*_via_sandbox_or_fallback` helpers for each
migrated syscall.

However, `mknodat` was explicitly deferred. Neither
`crates/fast_io/src/dir_sandbox/at_syscalls.rs` nor `DirSandbox` exposes
a `mknodat` or `mknodat_via_sandbox_or_fallback` helper. All device-node
and FIFO creation still uses path-based syscalls, either through
`rustix::fs::mknodat(CWD, ...)` (which passes `AT_FDCWD` - equivalent to
bare `mknod`) or through the `apple-fs` crate's `nix::sys::stat::mknod` /
`nix::unistd::mkfifo` wrappers.

---

## 1. Syscall surface inventory

### 1.1 Production call sites

| # | File | Line(s) | Syscall | Description |
|---|------|---------|---------|-------------|
| P1 | `crates/metadata/src/special.rs` | 153 | `rustix::fs::mknodat(CWD, destination, FileType::Fifo\|Socket, mode, makedev(0,0))` | Creates FIFO or socket node on Linux/non-Apple Unix |
| P2 | `crates/metadata/src/special.rs` | 201 | `rustix::fs::mknodat(CWD, destination, FileType::CharacterDevice\|BlockDevice, mode, device)` | Creates block/char device node on Linux/non-Apple Unix |
| P3 | `crates/metadata/src/special.rs` | 235 | `apple_fs::mknod(destination, S_IFSOCK\|mode, 0)` | Creates socket node on macOS/Apple via `nix::sys::stat::mknod` |
| P4 | `crates/metadata/src/special.rs` | 238 | `apple_fs::mkfifo(destination, mode)` | Creates FIFO on macOS/Apple via `nix::unistd::mkfifo` |
| P5 | `crates/metadata/src/special.rs` | 288 | `apple_fs::mknod(destination, mode, device)` | Creates block/char device on macOS/Apple via `nix::sys::stat::mknod` |
| P6 | `crates/apple-fs/src/lib.rs` | 43 | `nix::unistd::mkfifo(path, mode)` | Low-level FIFO creation (Apple wrapper) |
| P7 | `crates/apple-fs/src/lib.rs` | 49 | `nix::sys::stat::mknod(path, kind, perm, device)` | Low-level mknod (Apple wrapper) |

### 1.2 Fake-super placeholder path (no mknod issued)

When `--fake-super` is active, the `create_fifo_with_fake_super` and
`create_device_node_with_fake_super` functions bypass all mknod/mkfifo
syscalls and instead create a regular `0600` placeholder file via
`fs::OpenOptions::new().write(true).create_new(true).open(destination)`.
This path is at `crates/metadata/src/special.rs:93-110`
(`create_fake_super_placeholder`). It is path-based but does not involve
`mknod` - it is a standard `open(O_CREAT|O_EXCL)`.

### 1.3 Non-Unix stubs (no syscall issued)

| File | Line(s) | Behaviour |
|------|---------|-----------|
| `crates/metadata/src/special.rs` | 293-305 | `create_fifo_inner` and `create_device_node_inner` return `Ok(())` silently |
| `crates/apple-fs/src/lib.rs` | 137-155 | `mkfifo` and `mknod` return `Err(Unsupported)` |
| `crates/engine/src/local_copy/executor/special/device.rs` | 218-222 | `#[cfg(not(unix))]` block is a no-op comment |
| `crates/engine/src/local_copy/executor/special/fifo.rs` | 222-229 | `#[cfg(not(unix))]` block creates an empty file as a FIFO placeholder |

---

## 2. Call graph

### 2.1 FIFO creation - local-copy executor path

```
engine::local_copy::plan::CopyPlan::execute()
  -> engine::local_copy::executor::sources::copy_sources()
    -> orchestration::process_single_source() / handle_non_directory_source()
      -> handlers::handle_fifo_copy()
        -> engine::local_copy::executor::special::fifo::copy_fifo()
          -> metadata::create_fifo_with_fake_super(destination, metadata, fake_super)
            -> [fake_super=true]  create_fake_super_placeholder()       -- no mknod
            -> [fake_super=false] create_fifo_inner(destination, metadata)
              -> [Linux]  rustix::fs::mknodat(CWD, destination, ...)    -- P1
              -> [macOS]  apple_fs::mkfifo(destination, mode)           -- P4 -> P6
              -> [Windows] Ok(())                                       -- no-op
```

### 2.2 FIFO creation - recursive directory traversal path

```
engine::local_copy::executor::directory::recursive::copy_directory_recursive()
  -> recursive::entry::process_planned_entry()
    [EntryAction::CopyFifo]
      -> engine::local_copy::executor::special::fifo::copy_fifo()
        -> (same chain as 2.1 from copy_fifo onward)
```

### 2.3 FIFO creation - symlink copy-links dereference path

```
engine::local_copy::executor::special::symlink::copy_symlink()
  [--copy-links resolves symlink to a FIFO target]
    -> engine::local_copy::executor::special::fifo::copy_fifo()
      -> (same chain as 2.1 from copy_fifo onward)
```

### 2.4 Device node creation - local-copy executor path

```
engine::local_copy::plan::CopyPlan::execute()
  -> engine::local_copy::executor::sources::copy_sources()
    -> orchestration::process_single_source() / handle_non_directory_source()
      -> handlers::handle_device_copy()
        -> engine::local_copy::executor::special::device::copy_device()
          -> metadata::create_device_node_with_fake_super(destination, metadata, fake_super)
            -> [fake_super=true]  create_fake_super_placeholder()        -- no mknod
            -> [fake_super=false] create_device_node_inner(destination, metadata)
              -> [Linux]  rustix::fs::mknodat(CWD, destination, ...)     -- P2
              -> [macOS]  apple_fs::mknod(destination, mode, device)     -- P5 -> P7
              -> [Windows] Ok(())                                        -- no-op
```

### 2.5 Device node creation - recursive directory traversal path

```
engine::local_copy::executor::directory::recursive::copy_directory_recursive()
  -> recursive::entry::process_planned_entry()
    [EntryAction::CopyDevice]
      -> engine::local_copy::executor::special::device::copy_device()
        -> (same chain as 2.4 from copy_device onward)
```

### 2.6 Device node creation - symlink copy-links dereference path

```
engine::local_copy::executor::special::symlink::copy_symlink()
  [--copy-links resolves symlink to a device target]
    -> engine::local_copy::executor::special::device::copy_device()
      -> (same chain as 2.4 from copy_device onward)
```

### 2.7 Socket creation

Sockets follow the same path as FIFOs. `create_fifo_inner` on Linux
dispatches `rustix::fs::mknodat(CWD, ..., FileType::Socket, ...)` (P1)
and on macOS dispatches `apple_fs::mknod(destination, S_IFSOCK|mode, 0)`
(P3). The socket/FIFO distinction is resolved inside `create_fifo_inner`
based on `metadata.file_type().is_socket()`.

### 2.8 Backup path - no mknod

The backup module (`crates/engine/src/local_copy/executor/file/backup.rs`)
handles only regular files and symlinks. When a device or FIFO exists at
the destination before overwrite, the backup path uses `rename` (not
`mknod`). The `trace_make_backup_device` function in `backup_trace.rs` is
a logging trace only - it does not issue any syscall.

### 2.9 Receiver-side transfer crate - no direct mknod

The `crates/transfer/src/receiver/` code does not directly create device
nodes or FIFOs. The `is_device_target` path in
`crates/transfer/src/disk_commit/process.rs:235` is the `--write-devices`
feature which opens an existing device file for writing rather than
creating one via `mknod`. All special-file creation is delegated to the
engine crate through the local-copy executor.

---

## 3. Sandbox status per site

The SEC-1.e `DirSandbox` carrier is available in the `transfer` crate
receiver (`crates/transfer/src/receiver/mod.rs:674`) and is plumbed into
`DiskCommitConfig`, `TempFileGuard`, and directory-management helpers.
However, it is not available in the `engine` crate's local-copy executor
or the `metadata` crate's `special.rs` module.

| Site | Sandboxed? | Details |
|------|-----------|---------|
| P1 - `mknodat(CWD, ...)` FIFO/socket (Linux) | **Unsandboxed** | Uses `CWD` (`AT_FDCWD`), equivalent to bare path-based `mknod`. No `DirSandbox` in scope. |
| P2 - `mknodat(CWD, ...)` device (Linux) | **Unsandboxed** | Same `CWD` pattern. No `DirSandbox` in scope. |
| P3 - `apple_fs::mknod` socket (macOS) | **Unsandboxed** | Path-based `nix::sys::stat::mknod`. No dirfd variant available. |
| P4 - `apple_fs::mkfifo` FIFO (macOS) | **Unsandboxed** | Path-based `nix::unistd::mkfifo`. No dirfd variant available. |
| P5 - `apple_fs::mknod` device (macOS) | **Unsandboxed** | Path-based `nix::sys::stat::mknod`. No dirfd variant available. |
| P6 - `nix::unistd::mkfifo` (Apple wrapper) | **Unsandboxed** | Low-level implementation behind P4. |
| P7 - `nix::sys::stat::mknod` (Apple wrapper) | **Unsandboxed** | Low-level implementation behind P3 and P5. |

All seven production call sites are unsandboxed. The `DirSandbox` carrier
does not provide a `mknodat_via_sandbox_or_fallback` helper, and the
`metadata` crate has no access to any dirfd.

---

## 4. Platform coverage

| Site | Linux | macOS | Windows | `cfg` gate |
|------|-------|-------|---------|------------|
| P1 | Active | - | - | `#[cfg(all(unix, not(any(target_os = "ios", "macos", "tvos", "watchos"))))]` |
| P2 | Active | - | - | Same as P1 |
| P3 | - | Active | - | `#[cfg(all(unix, any(target_os = "ios", "macos", "tvos", "watchos")))]` |
| P4 | - | Active | - | Same as P3 |
| P5 | - | Active | - | Same as P3 |
| P6 | - | Active | - | `#[cfg(unix)]` inside `apple-fs` crate |
| P7 | - | Active | - | `#[cfg(unix)]` inside `apple-fs` crate |
| `create_fifo_inner` stub | - | - | No-op `Ok(())` | `#[cfg(not(unix))]` |
| `create_device_node_inner` stub | - | - | No-op `Ok(())` | `#[cfg(not(unix))]` |
| `copy_fifo` Windows fallback | - | - | Creates empty file | `#[cfg(not(unix))]` in `fifo.rs:222-229` |
| `copy_device` Windows fallback | - | - | No-op comment | `#[cfg(not(unix))]` in `device.rs:218-222` |
| `apple_fs::mkfifo` non-Unix stub | - | - | Returns `Unsupported` | `#[cfg(not(unix))]` |
| `apple_fs::mknod` non-Unix stub | - | - | Returns `Unsupported` | `#[cfg(not(unix))]` |

---

## 5. Migration difficulty

### 5.1 Linux sites (P1, P2) - Trivial

The Linux `create_fifo_inner` and `create_device_node_inner` functions
already use `rustix::fs::mknodat` - they just pass `CWD` instead of a
real dirfd. The migration requires:

1. Add a `mknodat_via_sandbox_or_fallback` helper to
   `crates/fast_io/src/dir_sandbox/at_syscalls.rs` following the existing
   pattern (accept `Option<&DirSandbox>`, extract leaf from path, call
   `rustix::fs::mknodat(dirfd, leaf, ...)` when sandboxed, fall back to
   `rustix::fs::mknodat(CWD, full_path, ...)` otherwise).

2. Plumb `Option<&DirSandbox>` (or `Option<BorrowedFd>`) through:
   - `metadata::create_fifo` / `metadata::create_fifo_with_fake_super`
   - `metadata::create_device_node` / `metadata::create_device_node_with_fake_super`
   - `engine::local_copy::executor::special::fifo::copy_fifo`
   - `engine::local_copy::executor::special::device::copy_device`

3. The `CopyContext` already carries metadata options; the sandbox fd
   would need to be added to the context or passed alongside it. The
   `DirSandbox` is currently in `crates/transfer/` but not in
   `crates/engine/`. Either the engine crate takes a dependency on
   `fast_io::DirSandbox`, or the dirfd is passed as a raw `BorrowedFd`.

**Effort estimate: trivial.** The `mknodat` syscall signature is already
used; only the fd argument changes. The `_via_sandbox_or_fallback` pattern
is well-established in `at_syscalls.rs` with 12 existing implementations
to follow.

### 5.2 macOS sites (P3, P4, P5) - Moderate

macOS supports `mknodat(2)` (available since macOS 13 / Ventura) but
neither the `nix` crate nor `apple-fs` exposes it. The migration requires:

1. Either use `rustix::fs::mknodat` (which wraps the libc `mknodat` on
   macOS) or add a direct `libc::mknodat` call in the `apple-fs` crate.

2. Replace the `nix::sys::stat::mknod` and `nix::unistd::mkfifo` calls
   with `mknodat(dirfd, leaf, ...)` variants.

3. Same plumbing requirement as the Linux path - the dirfd must reach the
   Apple-specific `create_fifo_inner` and `create_device_node_inner`.

4. Fallback needed for macOS 12 and earlier where `mknodat` may not be
   available. The `_via_sandbox_or_fallback` pattern handles this
   naturally.

**Effort estimate: moderate.** Requires verifying `mknodat` availability
on the minimum supported macOS version, and the `apple-fs` crate's
`nix`-based implementation needs reworking to accept a dirfd.

### 5.3 Fake-super placeholder path - Trivial

The `create_fake_super_placeholder` function at
`crates/metadata/src/special.rs:93-110` uses
`fs::OpenOptions::new().create_new(true).open(destination)`. This could
be migrated to `openat_via_sandbox_or_fallback` which already exists in
the `DirSandbox` API. Same plumbing requirement for the dirfd.

**Effort estimate: trivial.** The `openat` helper is already available.

### 5.4 Windows/non-Unix stubs - N/A

No actual filesystem syscall is issued. No migration needed.

---

## 6. Test-only call sites

These sites use `mknodat` or `mkfifo` to set up test fixtures. They do
not need sandbox migration but are listed for completeness.

| File | Line(s) | Syscall | Notes |
|------|---------|---------|-------|
| `crates/engine/src/local_copy/tests/mod.rs` | 38-45 | `rustix::fs::mknodat(CWD, ...)` | Linux test helper `mkfifo_for_tests` |
| `crates/engine/src/local_copy/tests/mod.rs` | 57-63 | `apple_fs::mkfifo(path, ...)` | macOS test helper `mkfifo_for_tests` |
| `crates/engine/src/local_copy/executor/directory/support.rs` | 292-303 | `rustix::fs::mknodat(CWD, ...)` | Test in `support.rs` |
| `crates/engine/src/delete/extras.rs` | 272 | `libc::mkfifo(c_path, 0o600)` | Test helper (unsafe, uses raw libc) |
| `crates/cli/src/frontend/tests/common.rs` | 151-158 | `rustix::fs::mknodat(CWD, ...)` | CLI test helper (Linux) |
| `crates/cli/src/frontend/tests/common.rs` | 170-176 | `apple_fs::mkfifo(path, ...)` | CLI test helper (macOS) |
| `crates/transfer/src/generator/tests.rs` | 1645 | `std::process::Command::new("mkfifo")` | Generator test via external command |

---

## 7. Summary

All production mknod/mkfifo call sites funnel through two functions in
`crates/metadata/src/special.rs`: `create_fifo_inner` and
`create_device_node_inner`. These are the sole points requiring migration
to `mknodat(dirfd, leaf, ...)` for SEC-1 sandbox coverage.

The call graph is narrow:
- **2 entry points** in the engine crate: `copy_fifo()` and `copy_device()`
- **3 callers** each: source handler, recursive directory traversal, and
  symlink copy-links dereference
- **1 bottom function** each: `create_fifo_inner` and
  `create_device_node_inner` in the metadata crate

The migration is straightforward on Linux (trivial - same syscall, just
swap `CWD` for a real dirfd) and moderate on macOS (needs `mknodat`
availability verification and `apple-fs` rework). The main plumbing work
is threading `Option<BorrowedFd>` or `Option<&DirSandbox>` from the
receiver's sandbox carrier down through the engine and metadata crates.
