# SEC-MK.d - Wire mknodat/mkfifoat into receiver special-file creation

- **Status**: OPEN
- **Date**: 2026-05-26
- **Predecessors**:
  - SEC-MK.a - mknod/mkfifo code-path inventory (`docs/audit/sec-mk-a-mknod-mkfifo-code-paths.md`)
  - SEC-MK.b - mknodat sandbox implementation in fast_io (`docs/design/sec-mk-b-mknodat-sandbox-impl.md`)
  - SEC-MK.c - mkfifoat sandbox implementation in fast_io
  - SEC-1.e - DirSandbox carrier wired through receiver pipeline
  - SEC-1.h - mkdirat/symlinkat/linkat sandbox helpers (template for this wiring)
- **Scope**: Replace the path-based `mknod`/`mkfifo` calls in the engine's
  special-file executor with dirfd-anchored `mknodat_via_sandbox_or_fallback`,
  closing the last TOCTOU gap for CVE-2026-29518/43619 on the
  device-node/FIFO creation surface.

---

## 1. Background

SEC-MK.a inventoried seven production call sites that create device nodes,
FIFOs, or sockets. All seven pass `CWD` (`AT_FDCWD`) on Linux or use
path-based `nix`/`apple_fs` wrappers on macOS. SEC-MK.b and SEC-MK.c added
`mknodat`, `mkfifoat`, and `mknodat_via_sandbox_or_fallback` helpers to
`crates/fast_io/src/dir_sandbox/at_syscalls.rs`, matching the twelve
existing `*_via_sandbox_or_fallback` helpers.

This spec covers the final wiring step: connecting the new helpers to the
receiver's special-file creation path so the `DirSandbox` dirfd is used
instead of `AT_FDCWD` when a sandbox is available.

---

## 2. Current call sites requiring migration

All production mknod/mkfifo syscalls funnel through two functions in
`crates/metadata/src/special.rs`:

| ID | Function | Syscall (Linux) | Syscall (macOS) |
|----|----------|-----------------|-----------------|
| P1 | `create_fifo_inner` | `rustix::fs::mknodat(CWD, destination, Fifo/Socket, mode, makedev(0,0))` | `apple_fs::mkfifo` / `apple_fs::mknod(S_IFSOCK)` |
| P2 | `create_device_node_inner` | `rustix::fs::mknodat(CWD, destination, Char/Block, mode, device)` | `apple_fs::mknod(S_IFCHR/S_IFBLK)` |

These are called from two engine-crate entry points:

| Entry point | File | Line | Calls |
|-------------|------|------|-------|
| `copy_fifo` | `crates/engine/src/local_copy/executor/special/fifo.rs` | 219 | `create_fifo_with_fake_super(destination, metadata, fake_super)` |
| `copy_device` | `crates/engine/src/local_copy/executor/special/device.rs` | 215 | `create_device_node_with_fake_super(destination, metadata, fake_super)` |

Each entry point has three callers:

1. Source handler (`handlers::handle_fifo_copy` / `handle_device_copy`)
2. Recursive directory traversal (`recursive::entry::process_planned_entry`)
3. Symlink copy-links dereference (`symlink::copy_symlink`)

---

## 3. Dirfd propagation path

The `DirSandbox` carrier is already threaded through the receiver pipeline:

```
ReceiverContext::setup_transfer()              # crates/transfer/src/receiver/transfer/setup.rs:163
  -> open_sandbox_for_dest(&dest_dir)          # returns Option<Arc<DirSandbox>>
  -> PipelineSetup { sandbox, ... }            # crates/transfer/src/receiver/mod.rs:674

PipelineSetup.sandbox                          # Option<Arc<DirSandbox>>
  -> run_sync() / run_pipelined() / etc.       # sandbox.as_deref() available
    -> create_symlinks(sandbox)                # SEC-1.h precedent
    -> create_hardlinks(sandbox)               # SEC-1.h precedent
    -> create_directories(sandbox)             # SEC-1.h precedent
```

However, the sandbox does NOT currently reach the engine crate's `copy_fifo`
and `copy_device` functions. The `CopyContext` struct
(`crates/engine/src/local_copy/mod.rs`) has no sandbox field.

### 3.1 Gap: receiver -> engine -> metadata

The call chain from receiver to the actual mknod syscall crosses three crate
boundaries:

```
transfer::receiver (has sandbox)
  -> engine::local_copy::executor::special (no sandbox)
    -> metadata::special (no sandbox, issues syscall)
```

### 3.2 Recommended approach: closure-based wiring at the engine layer

Following the pattern recommended in SEC-MK.b section 6.3, the sandbox
wiring uses a closure-based approach that avoids adding `fast_io` as a
dependency to the `metadata` crate:

1. The `engine` crate wraps the `create_fifo_with_fake_super` /
   `create_device_node_with_fake_super` call inside
   `fast_io::mknodat_via_sandbox_or_fallback`.
2. The existing metadata functions become the fallback closure.
3. The `metadata` crate's public API does not change.

This mirrors how `mkdirat_via_sandbox_or_fallback` hard-codes its fallback
to `std::fs::create_dir` at `at_syscalls.rs:608`, except the mknod fallback
is too complex for hard-coding (platform branching + fake-super) so a
closure carries it.

---

## 4. Before/after code snippets

### 4.1 FIFO creation (`copy_fifo`)

**Before** (`crates/engine/src/local_copy/executor/special/fifo.rs:214-229`):

```rust
// actually create a FIFO, or a 0600 placeholder when --fake-super is
// active (mirrors upstream syscall.c:do_mknod()'s am_root < 0 branch).
#[cfg(unix)]
{
    let fake_super = metadata_options.fake_super_enabled();
    create_fifo_with_fake_super(destination, metadata, fake_super)
        .map_err(map_metadata_error)?;
}
#[cfg(not(unix))]
{
    // Windows / non-Unix: no FIFO support in this crate path.
    fs::File::create(destination)
        .map_err(|error| LocalCopyError::io("create fifo placeholder", destination, error))?;
}
```

**After**:

```rust
// SEC-MK.d: create a FIFO through the sandbox dirfd when available,
// falling back to the existing path-based create when the sandbox is
// absent or the relative path has multiple components.
// upstream: syscall.c:do_mknod()
#[cfg(unix)]
{
    let fake_super = metadata_options.fake_super_enabled();
    fast_io::mknodat_via_sandbox_or_fallback(
        context.sandbox(),
        context.dest_dir(),
        relative.unwrap_or(Path::new("")),
        destination,
        compose_fifo_mode(metadata),
        0, // dev is 0 for FIFOs and sockets
        || {
            create_fifo_with_fake_super(destination, metadata, fake_super)
                .map_err(|e| io::Error::from(e))
        },
    )
    .map_err(|error| LocalCopyError::io("create fifo", destination, error))?;
}
#[cfg(not(unix))]
{
    // Windows / non-Unix: no FIFO support in this crate path.
    fs::File::create(destination)
        .map_err(|error| LocalCopyError::io("create fifo placeholder", destination, error))?;
}
```

The `compose_fifo_mode` helper extracts the `S_IFIFO`/`S_IFSOCK` type bits
and permission bits from the metadata, composing the raw `u32` that
`mknodat` expects:

```rust
/// Compose the mode argument for `mknodat` when creating a FIFO or socket.
///
/// Merges the file-type bits (`S_IFIFO` or `S_IFSOCK`) with the permission
/// bits from the source metadata.
// upstream: syscall.c:do_mknod() - mode composition
#[cfg(unix)]
fn compose_fifo_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    let type_bits: u32 = if metadata.file_type().is_socket() {
        libc::S_IFSOCK as u32
    } else {
        libc::S_IFIFO as u32
    };
    let perm_bits = metadata.permissions().mode() & 0o777;
    type_bits | perm_bits
}
```

### 4.2 Device node creation (`copy_device`)

**Before** (`crates/engine/src/local_copy/executor/special/device.rs:210-222`):

```rust
// create the actual device node, or a 0600 placeholder when --fake-super
// is active (mirrors upstream syscall.c:do_mknod()'s am_root < 0 branch).
#[cfg(unix)]
{
    let fake_super = metadata_options.fake_super_enabled();
    create_device_node_with_fake_super(destination, metadata, fake_super)
        .map_err(map_metadata_error)?;
}
#[cfg(not(unix))]
{
    // Windows / non-Unix: we can't actually create a device node.
}
```

**After**:

```rust
// SEC-MK.d: create a device node through the sandbox dirfd when available,
// falling back to the existing path-based create when the sandbox is
// absent or the relative path has multiple components.
// upstream: syscall.c:do_mknod()
#[cfg(unix)]
{
    let fake_super = metadata_options.fake_super_enabled();
    fast_io::mknodat_via_sandbox_or_fallback(
        context.sandbox(),
        context.dest_dir(),
        relative.unwrap_or(Path::new("")),
        destination,
        compose_device_mode(metadata),
        device_rdev(metadata),
        || {
            create_device_node_with_fake_super(destination, metadata, fake_super)
                .map_err(|e| io::Error::from(e))
        },
    )
    .map_err(|error| LocalCopyError::io("create device", destination, error))?;
}
#[cfg(not(unix))]
{
    // Windows / non-Unix: we can't actually create a device node.
}
```

With helpers:

```rust
/// Compose the mode argument for `mknodat` when creating a device node.
///
/// Merges the file-type bits (`S_IFCHR` or `S_IFBLK`) with the permission
/// bits from the source metadata.
// upstream: syscall.c:do_mknod() - mode composition for devices
#[cfg(unix)]
fn compose_device_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    let type_bits: u32 = if metadata.file_type().is_char_device() {
        libc::S_IFCHR as u32
    } else {
        libc::S_IFBLK as u32
    };
    let perm_bits = metadata.permissions().mode() & 0o777;
    type_bits | perm_bits
}

/// Extract the raw device number from metadata for `mknodat`.
// upstream: rsync.h:MAKEDEV() macro
#[cfg(unix)]
fn device_rdev(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.rdev()
}
```

### 4.3 Fake-super short-circuit

When `fake_super` is `true`, `create_fifo_with_fake_super` and
`create_device_node_with_fake_super` bypass mknod entirely and create a
regular `0600` placeholder via `open(O_CREAT|O_EXCL)`. The sandbox path
must NOT be taken in this case - the placeholder is a regular file, not a
device node, and `mknodat` would create the wrong thing.

The short-circuit is handled naturally by the closure-based approach:
`mknodat_via_sandbox_or_fallback` only calls `mknodat` on the dirfd fast
path. When the sandbox is present and the leaf resolves, it calls `mknodat`
directly. When fake-super is active, the **caller** must skip the sandbox
adaptor entirely and call `create_fifo_with_fake_super` /
`create_device_node_with_fake_super` directly.

Revised after-code with fake-super guard:

```rust
#[cfg(unix)]
{
    let fake_super = metadata_options.fake_super_enabled();
    if fake_super {
        // --fake-super creates a regular 0600 placeholder, not a device node.
        // upstream: syscall.c:do_mknod() am_root < 0 branch
        create_fifo_with_fake_super(destination, metadata, true)
            .map_err(map_metadata_error)?;
    } else {
        fast_io::mknodat_via_sandbox_or_fallback(
            context.sandbox(),
            context.dest_dir(),
            relative.unwrap_or(Path::new("")),
            destination,
            compose_fifo_mode(metadata),
            0,
            || {
                create_fifo_with_fake_super(destination, metadata, false)
                    .map_err(|e| io::Error::from(e))
            },
        )
        .map_err(|error| LocalCopyError::io("create fifo", destination, error))?;
    }
}
```

The same pattern applies to `copy_device`.

---

## 5. CopyContext plumbing

### 5.1 Add sandbox to CopyContext

The `CopyContext` struct (`crates/engine/src/local_copy/mod.rs`) needs a
sandbox field and accessor:

```rust
pub struct CopyContext {
    // ... existing fields ...

    /// SEC-MK.d: parent-dirfd carrier rooted at the destination tree.
    /// `None` on non-Unix, when the destination root cannot be opened,
    /// or when operating in local-copy mode without a daemon receiver.
    #[cfg(unix)]
    sandbox: Option<Arc<fast_io::DirSandbox>>,
}

impl CopyContext {
    /// Returns the sandbox carrier for dirfd-anchored syscalls.
    #[cfg(unix)]
    pub(crate) fn sandbox(&self) -> Option<&fast_io::DirSandbox> {
        self.sandbox.as_deref()
    }

    /// Returns the destination root directory path.
    pub(crate) fn dest_dir(&self) -> &Path {
        &self.dest_dir
    }
}
```

The `engine` crate already depends on `fast_io` (verified in
`crates/engine/Cargo.toml`), so no new dependency is introduced.

### 5.2 Wire sandbox from receiver to CopyContext

The `CopyContext` builder (or constructor) must accept the sandbox from
the receiver pipeline. The receiver's `PipelineSetup` already carries
`Option<Arc<fast_io::DirSandbox>>` (at `crates/transfer/src/receiver/mod.rs:674`).

The exact plumbing path depends on how the `CopyContext` is constructed.
In the receiver transfer loop, the sandbox is available as
`setup.sandbox.clone()` (an `Arc` clone) and can be passed into the
`CopyContext` constructor alongside the existing parameters.

For the local-copy executor path (no receiver, no daemon), the sandbox
is `None`. This preserves backward compatibility - the fallback closure
fires for every local-copy invocation, matching pre-migration behaviour.

### 5.3 Dependency graph impact

```
transfer (has DirSandbox via PipelineSetup)
  -> engine (gains DirSandbox via CopyContext)  # already depends on fast_io
    -> metadata (unchanged)                     # no new dependency
```

The `metadata` crate's public API is unchanged. The sandbox adaptation
happens entirely in the `engine` crate via the closure pattern.

---

## 6. Fallback behaviour

### 6.1 When sandbox is `None`

The `mknodat_via_sandbox_or_fallback` adaptor calls the fallback closure.
This occurs in:

- **Local-copy mode**: no daemon, no receiver, no sandbox. The existing
  `create_fifo_inner` / `create_device_node_inner` path runs unchanged.
- **Receiver with failed sandbox open**: destination root does not exist
  yet (first-run transfer). The path-based fallback runs, identical to
  pre-migration behaviour.

### 6.2 When relative path has multiple components

`single_component_leaf` returns `None` when the relative path contains
directory separators (e.g., `subdir/my.fifo`). The fallback fires. This
is consistent with all twelve existing `_via_sandbox_or_fallback` helpers.
A per-directory dirfd stack for multi-component paths is future work
(tracked by the SEC-1 carrier refactor).

### 6.3 When fake-super is active

The fake-super branch creates a regular `0600` placeholder file, not a
device node. It bypasses the sandbox adaptor entirely and calls
`create_fifo_with_fake_super(destination, metadata, true)` directly. The
placeholder could itself be migrated to `openat_via_sandbox_or_fallback`
(which already exists), but that is a separate, lower-priority item
(SEC-MK.b section 7 Phase 4).

---

## 7. Platform behaviour

### 7.1 Linux

`mknodat` is the primary path. The `mknodat_via_sandbox_or_fallback`
helper calls `libc::mknodat(dirfd, leaf, mode, dev)` when the sandbox is
present and the leaf resolves. Fallback is `rustix::fs::mknodat(CWD, ...)`
via the existing `create_fifo_inner` / `create_device_node_inner`.

### 7.2 macOS

`libc::mknodat` is available since macOS 11.0 (Big Sur). The sandbox path
calls the same `libc::mknodat` as Linux. Fallback on macOS is the existing
`apple_fs::mkfifo` / `apple_fs::mknod` wrapper, which uses path-based
`nix::unistd::mkfifo` / `nix::sys::stat::mknod`.

### 7.3 Windows

The entire `mknodat_via_sandbox_or_fallback` call is `#[cfg(unix)]`-gated.
On Windows, the existing `#[cfg(not(unix))]` blocks in `copy_fifo` and
`copy_device` are unchanged:

- `copy_fifo`: creates an empty file as a placeholder.
- `copy_device`: no-op.

No sandbox migration is needed on Windows - NTFS handle-based APIs
structurally sidestep the TOCTOU window (SEC-1.l audit).

---

## 8. Impact on --fake-super mode

The `--fake-super` mode is explicitly handled before the sandbox adaptor
(section 4.3). When `fake_super` is `true`:

1. `create_fifo_with_fake_super` / `create_device_node_with_fake_super`
   create a regular `0600` placeholder via `open(O_CREAT|O_EXCL)`.
2. The `mknodat` sandbox path is not entered.
3. The `store_fake_super` xattr write that follows is unchanged.

The placeholder creation itself is a candidate for migration to
`openat_via_sandbox_or_fallback`, but this is deferred as a separate
lower-priority item. The fake-super placeholder creates a regular file,
not a special file, so the TOCTOU risk profile is different (the
`O_CREAT|O_EXCL` combination already provides atomicity guarantees).

---

## 9. Files changed

| File | Change |
|------|--------|
| `crates/engine/src/local_copy/mod.rs` | Add `#[cfg(unix)] sandbox: Option<Arc<fast_io::DirSandbox>>` field and accessor to `CopyContext` |
| `crates/engine/src/local_copy/executor/special/fifo.rs` | Wrap `create_fifo_with_fake_super` in `mknodat_via_sandbox_or_fallback` with fake-super guard; add `compose_fifo_mode` helper |
| `crates/engine/src/local_copy/executor/special/device.rs` | Wrap `create_device_node_with_fake_super` in `mknodat_via_sandbox_or_fallback` with fake-super guard; add `compose_device_mode` and `device_rdev` helpers |
| `crates/engine/src/local_copy/executor/special/mod.rs` | Re-export new mode-composition helpers if shared across modules |
| Builder/constructor site for `CopyContext` | Thread `Option<Arc<DirSandbox>>` from receiver's `PipelineSetup` |

No changes to `crates/metadata/src/special.rs` or `crates/fast_io/`.

---

## 10. Testing strategy

### 10.1 SEC-MK.e - unit tests for wired path

Add tests in `crates/engine/src/local_copy/tests/execute_special.rs` and
`execute_specials.rs`:

- **FIFO via sandbox**: create a `TempDir`, open a `DirSandbox` on it,
  construct a `CopyContext` with the sandbox, call `copy_fifo`, verify the
  FIFO exists under the sandbox root via `fstatat_nofollow`.
- **Device via sandbox** (fake-super): same pattern but with
  `fake_super = true`, verify a regular `0600` placeholder is created
  (not a device node).
- **Fallback when sandbox is None**: call `copy_fifo` with
  `sandbox = None`, verify the FIFO is created via the path-based fallback.
- **Fallback for multi-component path**: call `copy_fifo` with a
  multi-component relative path, verify the fallback fires even when a
  sandbox is present.
- **Mode composition**: unit tests for `compose_fifo_mode` and
  `compose_device_mode` verifying correct type-bit and permission-bit
  merging for FIFOs, sockets, char devices, and block devices.

### 10.2 SEC-MK.f - TOCTOU resistance regression test

- Create a `TempDir` with a sandbox.
- Place a symlink at the FIFO leaf name pointing outside the sandbox.
- Call `copy_fifo` with the sandbox.
- Verify the call fails with `EEXIST` (the symlink occupies the name)
  rather than creating a FIFO at the symlink target.
- Repeat for `copy_device`.

### 10.3 Cross-platform compilation

- `cargo clippy --workspace --all-targets --all-features` on Linux, macOS,
  and Windows. The `#[cfg(unix)]` gating must not produce unused-import or
  dead-code warnings on any platform.
- The `#[cfg(not(unix))]` blocks in `copy_fifo` and `copy_device` must
  remain unchanged and compile cleanly.

### 10.4 Interop validation

- Run the existing interop suite (`tools/ci/run_interop.sh`) with
  `--devices --specials` flags to verify upstream rsync compatibility is
  preserved.
- The sandbox path produces byte-identical filesystem state to the
  fallback path (same `st_mode`, `st_rdev`).

---

## 11. Migration order

| Step | Description | Risk |
|------|-------------|------|
| 1 | Add `sandbox` field and accessor to `CopyContext` | Low - additive, defaulting to `None` |
| 2 | Wire `Option<Arc<DirSandbox>>` from receiver `PipelineSetup` to `CopyContext` construction | Low - plumbing only, no behavioural change |
| 3 | Add `compose_fifo_mode` helper to `fifo.rs` | Low - pure function, unit-tested independently |
| 4 | Wrap `copy_fifo` creation in `mknodat_via_sandbox_or_fallback` with fake-super guard | Medium - the critical migration |
| 5 | Add `compose_device_mode` and `device_rdev` helpers to `device.rs` | Low - pure functions |
| 6 | Wrap `copy_device` creation in `mknodat_via_sandbox_or_fallback` with fake-super guard | Medium - same pattern as step 4 |
| 7 | Add tests (SEC-MK.e scope) | Low - additive |
| 8 | Add TOCTOU regression test (SEC-MK.f scope) | Low - additive |

Steps 1-2 are backward-compatible: when `sandbox` is `None` (the default
for local-copy mode and receivers that fail to open the sandbox), the
fallback closure fires and behaviour is identical to pre-migration code.

---

## 12. Risks and mitigations

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| Fake-super path accidentally routed through `mknodat` (would create device instead of placeholder) | Low | Explicit `if fake_super` guard before the sandbox adaptor; unit test verifying placeholder creation under fake-super |
| `CopyContext` lifetime issue with `Arc<DirSandbox>` | Low | `Arc` avoids lifetime threading; the sandbox outlives the entire transfer |
| Mode composition diverges from `metadata::special` logic | Medium | Extract shared `compose_mknod_mode` helper (or test parity between engine and metadata mode values) |
| macOS `mode_t` truncation (`u32` to `u16`) | Low | Existing `mkdirat` and `fchmodat` helpers use the same cast; mode values never exceed 16 bits |
| `dev_t` truncation on macOS (`u64` to `i32`) | Low | Add checked cast with `TryFrom`; device numbers on macOS fit in 32 bits |

---

## 13. Relationship to open SEC-1 deferrals

This task resolves **SEC-1.h closure item 1** ("mknodat for device / FIFO /
socket nodes") from `docs/design/sec-1-h-mknodat-deferral-2026-05-21.md`.
The re-open trigger was: "metadata crate gains DirSandbox plumbing for an
unrelated reason." SEC-MK.d avoids this trigger entirely by using the
closure-based approach at the engine layer, keeping the metadata crate
unchanged.

After SEC-MK.d ships, the remaining open SEC-1 deferrals are:

1. **SEC-1.i** - receiver wiring for `fchmodat`/`fchownat`/`utimensat`
   (6 sites). Still blocked on the metadata-crate carrier refactor.
2. **SEC-1.j** - receiver wiring for `renameat` (2 of 3 sites). Still
   blocked on cross-thread `DirSandbox` carrier.

---

## 14. Upstream rsync reference

- `syscall.c:do_mknod()` (lines 90-174) - the upstream C implementation
  this mirrors. The `am_root < 0` branch handles `--fake-super` placeholder
  substitution.
- `receiver.c` - device and FIFO creation during file-list application.
- `rsync.h:MAKEDEV()` macro - device number composition from major/minor.
