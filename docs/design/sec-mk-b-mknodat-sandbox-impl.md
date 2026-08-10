# SEC-MK.b - mknodat/mkfifoat sandbox implementation spec

- **Status**: OPEN
- **Date**: 2026-05-26
- **Predecessor**: SEC-MK.a (`docs/audit/sec-mk-a-mknod-mkfifo-code-paths.md`)
  - SEC-1.h mknodat deferral (`docs/design/sec-1-h-mknodat-deferral-2026-05-21.md`)
- **Scope**: Add `mknodat` and `mkfifoat` helpers to the `DirSandbox`
  `*at` syscall surface, then migrate all seven production call sites
  in `crates/metadata/src/special.rs` from path-based `mknod`/`mkfifo`
  to dirfd-anchored variants.

---

## 1. Background

The SEC-MK.a audit inventoried seven production call sites that create
device nodes, FIFOs, or sockets. All seven are unsandboxed - they pass
`CWD` (`AT_FDCWD`) on Linux or use path-based `nix::sys::stat::mknod` /
`nix::unistd::mkfifo` on macOS. The `DirSandbox` carrier in
`crates/fast_io/src/dir_sandbox/` already provides twelve `*at` helpers
(`fstatat`, `unlinkat`, `mkdirat`, `symlinkat`, `linkat`, `renameat`,
`fchmodat`, `fchownat`, `utimensat`, `openat`, `readlinkat`,
`recursive_unlinkat`) but has no `mknodat` equivalent.

This spec covers the complete implementation: new helpers in
`at_syscalls.rs`, API signature, platform behaviour, fallback strategy,
dirfd threading from the receiver through the engine into
`metadata::special`, migration order, and testing.

---

## 2. New helpers to add

Two new public functions in
`crates/fast_io/src/dir_sandbox/at_syscalls.rs`:

### 2.1 Raw syscall wrapper: `mknodat`

```rust
/// Issue `mknodat(dirfd, name, mode, dev)`.
///
/// Creates a filesystem node (FIFO, socket, character device, or block
/// device) beneath `dirfd`. The type bits (`S_IFIFO`, `S_IFSOCK`,
/// `S_IFCHR`, `S_IFBLK`) are encoded in `mode` alongside the permission
/// bits. `dev` carries the device major/minor for block and character
/// devices; it is ignored for FIFOs and sockets.
///
/// `name` must not contain an interior NUL byte; callers that pull names
/// from `Path::file_name` cannot trigger this.
///
/// # Errors
///
/// Surfaces the underlying syscall error verbatim. Notable cases:
/// - `EEXIST` when `name` already exists beneath `dirfd`.
/// - `EPERM` when creating device nodes without `CAP_MKNOD`.
/// - `ENOTDIR` when `dirfd` is not a directory.
/// - `EACCES` when the caller lacks write permission on `dirfd`.
/// - `EINVAL` when `name` contains an interior NUL byte.
pub fn mknodat(
    dirfd: BorrowedFd<'_>,
    name: &OsStr,
    mode: u32,
    dev: u64,
) -> io::Result<()>
```

**Implementation**: follows the exact pattern of `mkdirat`:

```rust
pub fn mknodat(
    dirfd: BorrowedFd<'_>,
    name: &OsStr,
    mode: u32,
    dev: u64,
) -> io::Result<()> {
    let c_name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;

    // SAFETY:
    // - `dirfd.as_raw_fd()` returns the raw fd of a `BorrowedFd<'_>`
    //   whose lifetime is bound to the borrow and outlives the syscall.
    // - `c_name.as_ptr()` is a valid NUL-terminated C string borrowed
    //   for the duration of the call; the kernel does not retain the
    //   pointer past return.
    // - `mode` carries both type bits (S_IFIFO, S_IFCHR, etc.) and
    //   permission bits; the cast to `mode_t` is safe because the
    //   caller already composes mode from `libc` constants.
    // - `dev` is cast to `dev_t`; on Linux this is a u64 identity,
    //   on macOS it is i32 (the cast truncates, but device numbers
    //   on macOS fit in i32).
    #[allow(unsafe_code)]
    let rc = unsafe {
        libc::mknodat(
            dirfd.as_raw_fd(),
            c_name.as_ptr(),
            mode as libc::mode_t,
            dev as libc::dev_t,
        )
    };

    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
```

### 2.2 Sandbox-or-fallback adaptor: `mknodat_via_sandbox_or_fallback`

```rust
/// Issue `mknodat` against `node_path` when the `sandbox` root is the
/// immediate parent.
///
/// SEC-MK.b adaptor for callers that already have an absolute path:
/// - When `sandbox` is `Some`, `node_path` equals
///   `dest_dir.join(relative_path)`, and `relative_path` has a single
///   component, the helper resolves the leaf through the sandbox dirfd
///   so a mid-syscall symlink swap on the leaf cannot redirect the
///   create to an attacker-chosen parent.
/// - In every other case the helper falls back to path-based mknod
///   via `fallback`, preserving non-daemon codepaths, --fake-super
///   placeholder substitution, and Apple-platform mknod wrappers.
///
/// The `fallback` closure abstracts over the platform-specific and
/// fake-super-aware creation that `metadata::special` performs.
/// This keeps the `fast_io` crate free of `metadata` dependencies.
///
/// # Errors
///
/// Surfaces either the `mknodat` error or the `fallback` error
/// verbatim, depending on which path was taken.
pub fn mknodat_via_sandbox_or_fallback<F>(
    sandbox: Option<&super::DirSandbox>,
    dest_dir: &Path,
    relative_path: &Path,
    node_path: &Path,
    mode: u32,
    dev: u64,
    fallback: F,
) -> io::Result<()>
where
    F: FnOnce() -> io::Result<()>,
```

**Why a closure-based fallback**: unlike `mkdirat_via_sandbox_or_fallback`
whose fallback is a trivial `std::fs::create_dir(dir_path)`, the mknod
fallback involves platform-specific branching (rustix on Linux, apple-fs
on macOS, no-op on Windows) and `--fake-super` placeholder substitution.
Embedding that logic in `fast_io` would create a dependency from `fast_io`
to `metadata`, violating the crate dependency graph
(`core -> engine -> protocol -> metadata`; `fast_io` is a peer of
`metadata`, not a dependent). The closure lets `metadata::special` supply
its own fallback while `fast_io` owns only the dirfd-anchored path.

**Implementation**:

```rust
pub fn mknodat_via_sandbox_or_fallback<F>(
    sandbox: Option<&super::DirSandbox>,
    dest_dir: &Path,
    relative_path: &Path,
    node_path: &Path,
    mode: u32,
    dev: u64,
    fallback: F,
) -> io::Result<()>
where
    F: FnOnce() -> io::Result<()>,
{
    if let Some(sandbox) = sandbox
        && let Some(leaf) = single_component_leaf(dest_dir, relative_path, node_path)
    {
        return mknodat(sandbox.current_dirfd(), leaf, mode, dev);
    }
    fallback()
}
```

### 2.3 No separate `mkfifoat` raw helper

A dedicated `mkfifoat` raw helper is unnecessary. `mknodat` with
`mode = S_IFIFO | permission_bits` and `dev = 0` is the POSIX way to
create a FIFO via dirfd - there is no separate `mkfifoat` syscall in
Linux or macOS. The convenience of a named `mkfifoat` function can be
provided at the `metadata::special` call site by composing the mode
argument. This avoids API surface bloat in `at_syscalls.rs`.

If call-site clarity demands a named wrapper, it can be a thin inline:

```rust
/// Convenience wrapper: creates a FIFO beneath `dirfd`.
///
/// Equivalent to `mknodat(dirfd, name, S_IFIFO | perm_bits, 0)`.
#[inline]
pub fn mkfifoat(
    dirfd: BorrowedFd<'_>,
    name: &OsStr,
    perm_bits: u32,
) -> io::Result<()> {
    mknodat(dirfd, name, libc::S_IFIFO as u32 | perm_bits, 0)
}
```

**Recommendation**: ship the thin `mkfifoat` wrapper for readability at
the call site. The `_via_sandbox_or_fallback` adaptor is shared - callers
compose `mode` with the appropriate type bits before calling
`mknodat_via_sandbox_or_fallback`.

---

## 3. API signature rationale

The signature of `mknodat_via_sandbox_or_fallback` follows the existing
pattern established by the twelve `*_via_sandbox_or_fallback` helpers:

| Parameter | Source | Notes |
|-----------|--------|-------|
| `sandbox: Option<&DirSandbox>` | Receiver pipeline | `None` in non-daemon codepaths, `Some` when receiver has a sandbox |
| `dest_dir: &Path` | Receiver's destination root | Used by `single_component_leaf` to verify the leaf relationship |
| `relative_path: &Path` | File-list entry's relative path | Must be single-component for the dirfd path to activate |
| `node_path: &Path` | `dest_dir.join(relative_path)` | The absolute path, used for the fallback |
| `mode: u32` | Source metadata | Type bits (`S_IFIFO`, `S_IFCHR`, etc.) OR'd with permission bits |
| `dev: u64` | Source metadata (`rdev`) | Device major/minor; 0 for FIFOs and sockets |
| `fallback: F` | Caller-supplied closure | Platform-specific and fake-super-aware creation logic |

The `fallback` closure parameter is the one divergence from the existing
pattern. Other `_via_sandbox_or_fallback` helpers hard-code their
fallback (e.g., `std::fs::create_dir` for `mkdirat`, `std::fs::symlink`
for `symlinkat`). The mknod fallback is too complex for hard-coding
without introducing cross-crate dependencies.

---

## 4. Platform behaviour

### 4.1 Linux

`libc::mknodat` is universally available on all supported Linux kernel
versions (added in Linux 2.6.16). The dirfd path is the primary path;
no OS-version gating is needed.

**Sandbox path**: `mknodat(sandbox.current_dirfd(), leaf, mode, dev)`.

**Fallback path**: `rustix::fs::mknodat(CWD, full_path, file_type, mode, makedev(maj, min))` - the existing `create_fifo_inner` / `create_device_node_inner` code.

### 4.2 macOS

`mknodat(2)` is available since macOS 11.0 (Big Sur, released 2020).
The minimum deployment target for oc-rsync on macOS is 11.0, so
`libc::mknodat` is available on all supported macOS versions.

**Sandbox path**: `libc::mknodat(sandbox.current_dirfd(), leaf, mode, dev)`.
The `dev_t` type on macOS is `i32`, so the `dev: u64` argument is cast
via `as libc::dev_t`. Device numbers on macOS fit in 32 bits.

**Fallback path**: The existing Apple-specific `create_fifo_inner` and
`create_device_node_inner` functions in `metadata::special`, which use
`apple_fs::mkfifo` (wrapping `nix::unistd::mkfifo`) and
`apple_fs::mknod` (wrapping `nix::sys::stat::mknod`).

**Note on `mode_t`**: `libc::mode_t` is `u16` on macOS. The `mode: u32`
parameter is cast via `as libc::mode_t`, which truncates the upper 16
bits. This is safe because POSIX mode values never exceed 16 bits
(`S_IFIFO | 0o7777` = `0o17777`, which fits in `u16`). This matches the
existing `mkdirat` and `fchmodat` helpers.

### 4.3 Windows (no-op stub)

The raw `mknodat` and `mkfifoat` helpers are `#[cfg(unix)]`-gated and
do not compile on Windows. The `mknodat_via_sandbox_or_fallback` adaptor
is also `#[cfg(unix)]`-gated (the entire `dir_sandbox` module is
`#[cfg(unix)]`).

On Windows, the existing no-op stubs in `metadata::special` apply:
`create_fifo_inner` and `create_device_node_inner` return `Ok(())`
silently. No sandbox migration is needed.

---

## 5. Fallback strategy

When `dirfd` is `None` (no sandbox available), the adaptor calls the
`fallback` closure, which the caller constructs from the existing
`metadata::special` functions. This preserves:

1. **Non-daemon codepaths**: local-copy mode does not carry a sandbox.
   The fallback is the existing `create_fifo_inner` /
   `create_device_node_inner` path, unchanged.

2. **`--fake-super` placeholder substitution**: when `fake_super` is
   `true`, `create_fifo_with_fake_super` and
   `create_device_node_with_fake_super` bypass mknod entirely and create
   a regular `0600` placeholder file. This logic stays in the metadata
   crate; the sandbox adaptor is not involved because the placeholder
   path uses `open(O_CREAT|O_EXCL)`, not `mknod`.

3. **Multi-component paths**: when `relative_path` has more than one
   component, `single_component_leaf` returns `None` and the fallback
   fires. This is consistent with all other `_via_sandbox_or_fallback`
   helpers. A per-directory dirfd stack for multi-component paths is
   tracked as future SEC-1 work.

4. **Apple platform wrappers**: the fallback closure can call
   `apple_fs::mkfifo` or `apple_fs::mknod` when compiled on macOS,
   preserving the existing platform dispatch.

---

## 6. Threading the dirfd

### 6.1 Current state

The `DirSandbox` is created in the `transfer` crate receiver
(`crates/transfer/src/receiver/transfer/setup.rs:193`,
`open_sandbox_for_dest`) and stored as `Option<Arc<DirSandbox>>` on the
receiver struct (`crates/transfer/src/receiver/mod.rs:674`). From there
it is passed to:

- `receiver::directory::creation` (for `mkdirat_via_sandbox_or_fallback`)
- `receiver::directory::links` (for `linkat_via_sandbox_or_fallback`)
- `receiver::directory::deletion` (for `unlink_via_sandbox_or_fallback`,
  `recursive_unlinkat_via_sandbox_or_fallback`)
- `engine::delete::DeleteEmitter` (for SEC-1.q sandbox-anchored deletes)

The `engine::local_copy` executor and `metadata::special` module have
**no** access to the sandbox today. The `CopyContext` struct in
`crates/engine/src/local_copy/mod.rs` does not carry a sandbox field.

### 6.2 Plumbing plan

Thread `Option<BorrowedFd<'_>>` (not `Option<&DirSandbox>`) through the
call chain. Using `BorrowedFd` instead of `&DirSandbox` avoids adding a
`fast_io` dependency to the `metadata` crate. The `metadata` crate stays
dependency-free of `fast_io`; the adaptation happens at the `engine`
layer.

#### Step 1: Add `dirfd` parameter to `metadata::special` functions

```rust
// crates/metadata/src/special.rs

#[cfg(unix)]
pub fn create_fifo_with_fake_super(
    destination: &Path,
    metadata: &fs::Metadata,
    fake_super: bool,
    dirfd: Option<BorrowedFd<'_>>,  // NEW
) -> Result<(), MetadataError>

#[cfg(unix)]
pub fn create_device_node_with_fake_super(
    destination: &Path,
    metadata: &fs::Metadata,
    fake_super: bool,
    dirfd: Option<BorrowedFd<'_>>,  // NEW
) -> Result<(), MetadataError>
```

When `dirfd` is `Some`, and the destination path has a single filename
component (no directory separators), use `libc::mknodat(dirfd, leaf, ...)`
directly. When `dirfd` is `None`, fall back to the existing path-based
implementation.

The fake-super branch (`create_fake_super_placeholder`) is unaffected -
it creates a regular file via `OpenOptions`, not `mknod`. It can be
migrated to `openat` separately (the `openat_via_sandbox_or_fallback`
helper already exists).

#### Step 2: Add `sandbox` field to `CopyContext`

```rust
// crates/engine/src/local_copy/mod.rs

pub struct CopyContext {
    // ... existing fields ...
    #[cfg(unix)]
    sandbox_dirfd: Option<BorrowedFd<'static>>,
    // ...
}
```

The lifetime is `'static` because the `Arc<DirSandbox>` owned by the
receiver outlives the entire transfer. The `BorrowedFd` is derived from
`sandbox.current_dirfd()` at the point where `CopyContext` is
constructed.

#### Step 3: Pass `sandbox_dirfd` through executor functions

```
copy_fifo(context, ...) -> create_fifo_with_fake_super(..., context.sandbox_dirfd())
copy_device(context, ...) -> create_device_node_with_fake_super(..., context.sandbox_dirfd())
```

#### Step 4: Wire from receiver to `CopyContext`

The receiver constructs `CopyContext` (or its builder) with the sandbox
dirfd obtained from `self.sandbox.as_ref().map(|s| s.current_dirfd())`.

### 6.3 Alternative: closure-based approach (recommended)

Instead of threading `BorrowedFd` through `metadata::special`, use the
closure-based `mknodat_via_sandbox_or_fallback` at the **engine** layer.
The engine's `copy_fifo` and `copy_device` functions already have access
to both the destination path and the sandbox (once Step 2 wires it).
They call `mknodat_via_sandbox_or_fallback` with a fallback closure that
invokes the existing `metadata::create_fifo_with_fake_super` /
`metadata::create_device_node_with_fake_super`:

```rust
// crates/engine/src/local_copy/executor/special/fifo.rs

#[cfg(unix)]
{
    let fake_super = metadata_options.fake_super_enabled();
    mknodat_via_sandbox_or_fallback(
        context.sandbox(),
        context.dest_dir(),
        relative.unwrap_or(Path::new("")),
        destination,
        compose_fifo_mode(metadata),
        0, // dev is 0 for FIFOs
        || {
            create_fifo_with_fake_super(destination, metadata, fake_super)
                .map_err(|e| e.into())
        },
    )?;
}
```

**Advantage**: the `metadata` crate's public API does not change. No
`BorrowedFd` parameter is added to `create_fifo_with_fake_super` or
`create_device_node_with_fake_super`. The sandbox awareness is contained
in the `engine` crate, which already depends on `fast_io`.

**Disadvantage**: the mode composition (merging type bits with permission
bits) must be done at the engine layer, duplicating some logic that
`create_fifo_inner` / `create_device_node_inner` already perform.

**Recommendation**: use the closure-based approach. The mode composition
is straightforward and can be extracted into a `compose_mknod_mode`
helper in `metadata::special` that is reused by both the sandbox path
and the fallback path.

---

## 7. Migration plan

### Phase 1: Add helpers to `at_syscalls.rs`

1. Add `mknodat(dirfd, name, mode, dev)` raw wrapper.
2. Add `mkfifoat(dirfd, name, perm_bits)` convenience wrapper.
3. Add `mknodat_via_sandbox_or_fallback` adaptor with closure fallback.
4. Re-export from `crates/fast_io/src/dir_sandbox/mod.rs`.
5. Update module docstring to list SEC-MK.b alongside SEC-1.h.
6. Unit tests for the raw wrappers (FIFO creation in a tmpdir).

### Phase 2: Wire sandbox into engine `CopyContext`

1. Add `#[cfg(unix)] sandbox: Option<&DirSandbox>` (or
   `Option<Arc<DirSandbox>>`) to `CopyContext` or its builder.
2. Add accessor method `sandbox(&self) -> Option<&DirSandbox>`.
3. Add `dest_dir(&self) -> &Path` accessor if not already present.
4. Wire from receiver's `open_sandbox_for_dest` through to `CopyContext`
   construction.

### Phase 3: Migrate call sites

Migrate in order from lowest risk to highest:

| Order | Call site | Change |
|-------|-----------|--------|
| 1 | `copy_fifo` (FIFO, Linux) | Wrap `create_fifo_with_fake_super` call in `mknodat_via_sandbox_or_fallback` with fallback closure |
| 2 | `copy_device` (device, Linux) | Same pattern for `create_device_node_with_fake_super` |
| 3 | `copy_fifo` (FIFO, macOS) | Same pattern; `mknodat` on macOS >= 11.0, fallback to `apple_fs::mkfifo` |
| 4 | `copy_device` (device, macOS) | Same pattern; fallback to `apple_fs::mknod` |
| 5 | Socket creation (Linux + macOS) | Sockets flow through `create_fifo_inner`; covered by items 1 and 3 |

**All changes are backward-compatible**: when `sandbox` is `None` (the
current state for all codepaths), the fallback closure fires and behaviour
is identical to the pre-migration code.

### Phase 4: Migrate fake-super placeholder path (optional, lower priority)

The `create_fake_super_placeholder` function uses
`fs::OpenOptions::new().create_new(true).open(destination)`. This can
be migrated to `openat_via_sandbox_or_fallback` which already exists.
This is a separate, lower-priority item because the placeholder path
creates a regular file (not a device node) and does not exercise `mknod`.

---

## 8. Testing strategy

### 8.1 FIFO creation (no root required)

FIFOs can be created by any user. Tests create a `TempDir`, open a
`DirSandbox` on it, and verify:

- `mknodat(dirfd, "test.fifo", S_IFIFO | 0o644, 0)` creates a FIFO
  visible via `fstatat_nofollow`.
- `mkfifoat(dirfd, "test.fifo", 0o644)` produces the same result.
- `mknodat_via_sandbox_or_fallback` with `Some(sandbox)` and a
  single-component relative path uses the dirfd path (verify by checking
  the FIFO exists under the sandbox root).
- `mknodat_via_sandbox_or_fallback` with `None` calls the fallback
  closure (verify by asserting the closure was invoked via a flag or
  `AtomicBool`).
- `mknodat_via_sandbox_or_fallback` with a multi-component relative path
  calls the fallback even when `sandbox` is `Some`.

### 8.2 Socket creation (no root required)

Sockets can also be created by any user via `mknod(S_IFSOCK)`:

- `mknodat(dirfd, "test.sock", S_IFSOCK | 0o644, 0)` creates a socket
  node visible via `fstatat_nofollow`.

### 8.3 Device node creation (requires root or `--fake-super`)

Device nodes require `CAP_MKNOD` (Linux) or root (macOS). Tests use
`--fake-super` to test the placeholder path without privileges:

- With `fake_super = true`, the fallback closure creates a regular `0600`
  placeholder file instead of calling `mknodat`. Verify the placeholder
  exists and is a regular file.
- With `fake_super = false` and without root, `mknodat` returns `EPERM`.
  Verify the error is propagated correctly.
- CI runs as root in the interop container - add a `#[cfg_attr(not(root), ignore)]`
  test (or equivalent gating) that creates a real char device node under
  the sandbox dirfd and verifies `fstatat_nofollow` reports `S_IFCHR`.

### 8.4 TOCTOU resistance (integration)

- Create a `TempDir` with a sandbox.
- Place a symlink at the leaf name pointing outside the sandbox.
- Call `mknodat(dirfd, symlink_leaf, S_IFIFO | 0o644, 0)`.
- Verify the call fails with `EEXIST` (the symlink occupies the name)
  rather than creating a FIFO at the symlink target.

### 8.5 Fallback parity

- For each platform (Linux, macOS), verify that the sandbox path and the
  fallback path produce byte-identical filesystem state (same `st_mode`,
  same `st_rdev`, same `st_dev`/`st_ino` relationship to the parent
  directory).

### 8.6 Cross-platform compilation

- Verify `cargo clippy --workspace --all-targets --all-features` passes
  on Linux, macOS, and Windows. The `#[cfg(unix)]` gating must not
  produce unused-import or dead-code warnings on any platform.

---

## 9. Files changed

| File | Change |
|------|--------|
| `crates/fast_io/src/dir_sandbox/at_syscalls.rs` | Add `mknodat`, `mkfifoat`, `mknodat_via_sandbox_or_fallback` |
| `crates/fast_io/src/dir_sandbox/mod.rs` | Re-export new helpers; update module docstring |
| `crates/fast_io/src/dir_sandbox/tests.rs` | Add unit tests for new helpers |
| `crates/engine/src/local_copy/mod.rs` | Add `#[cfg(unix)] sandbox` field to `CopyContext` |
| `crates/engine/src/local_copy/executor/special/fifo.rs` | Wrap `create_fifo_with_fake_super` in sandbox adaptor |
| `crates/engine/src/local_copy/executor/special/device.rs` | Wrap `create_device_node_with_fake_super` in sandbox adaptor |
| `crates/metadata/src/special.rs` | Extract `compose_mknod_mode` helper (optional, for mode composition reuse) |

---

## 10. Risks and mitigations

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| `mode_t` truncation on macOS (`u32` -> `u16`) drops type bits | Low - `S_IFIFO \| 0o7777` fits in `u16` | Existing `mkdirat` and `fchmodat` use the same cast; add a debug assertion that `mode <= 0o177777` |
| `dev_t` truncation on macOS (`u64` -> `i32`) corrupts device number | Low - macOS device numbers fit in 32 bits | Add a checked cast with `TryFrom` and return `EINVAL` on overflow, matching the existing `invalid_device_error` in `special.rs` |
| `CopyContext` lifetime issue with `BorrowedFd` | Medium | Use the closure-based approach (section 6.3) to avoid lifetime threading entirely |
| Fallback closure captures `&fs::Metadata` across sandbox dispatch | Low | The closure borrows only stack-local references; no lifetime extension needed |

---

## 11. Upstream rsync reference

- `syscall.c:do_mknod()` (lines 90-174) - the upstream implementation
  this mirrors. The `am_root < 0` branch handles `--fake-super`
  placeholder substitution.
- `receiver.c` - device and FIFO creation during file-list application.
- `rsync.h:MAKEDEV()` macro - device number composition from major/minor.
