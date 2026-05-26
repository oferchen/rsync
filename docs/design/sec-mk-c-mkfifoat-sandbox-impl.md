# SEC-MK.c - mkfifoat sandbox implementation spec

- **Status**: OPEN
- **Date**: 2026-05-26
- **Predecessor**: SEC-MK.b (`docs/design/sec-mk-b-mknodat-sandbox-impl.md`)
- **Scope**: Add a dedicated `mkfifoat` helper to the `DirSandbox`
  `*at` syscall surface in `crates/fast_io/src/dir_sandbox/at_syscalls.rs`,
  integrate it with the receiver pipeline, and migrate the FIFO
  creation call sites in `crates/metadata/src/special.rs` from
  path-based `mkfifo`/`mknod(S_IFIFO)` to dirfd-anchored variants.

---

## 1. Background

SEC-MK.b designed the general `mknodat` sandbox helper and its
closure-based `mknodat_via_sandbox_or_fallback` adaptor. That spec
covers both device nodes and FIFOs under a single `mknodat` umbrella,
noting that `mknodat(dirfd, name, S_IFIFO | perm, 0)` is the POSIX way
to create a FIFO via dirfd.

This spec separates mkfifoat into its own document for three reasons:

1. **macOS exposes `mkfifoat` as a distinct API.** While Linux creates
   FIFOs through `mknodat(S_IFIFO)`, macOS has provided `mkfifoat(2)`
   as a separate syscall since macOS 10.13 (High Sierra). Using the
   native API is more idiomatic and avoids mode-bit composition on
   Apple targets.

2. **Testing is simpler.** FIFO creation requires no special privileges -
   any unprivileged user can create a FIFO. Device node creation
   requires `CAP_MKNOD` (Linux) or root (macOS), forcing tests through
   `--fake-super` placeholder substitution or CI root containers.
   mkfifoat tests can exercise the real syscall path in every CI
   environment without privilege escalation.

3. **The fallback path differs from device nodes.** On platforms where
   `MKNOD_CREATES_FIFOS` is not defined, upstream rsync's
   `syscall.c:do_mknod()` routes FIFOs through `mkfifo(2)` instead
   of `mknod(2)`. The Apple branch of `create_fifo_inner` in
   `metadata::special` already does this via `apple_fs::mkfifo`. A
   dedicated `mkfifoat` helper encapsulates this platform divergence
   cleanly.

---

## 2. Relationship to SEC-MK.b

SEC-MK.b defines two primitives:

- `mknodat(dirfd, name, mode, dev)` - raw syscall wrapper.
- `mknodat_via_sandbox_or_fallback(sandbox, dest_dir, relative_path,
  node_path, mode, dev, fallback)` - closure-based adaptor.

mkfifoat can reuse the SEC-MK.b adaptor by composing mode as
`S_IFIFO | perm_bits` and passing `dev = 0`. The two specs share:

| Component | Shared? | Notes |
|-----------|---------|-------|
| `mknodat` raw wrapper | Yes | mkfifoat calls `mknodat(dirfd, name, S_IFIFO \| perm, 0)` on Linux |
| `mknodat_via_sandbox_or_fallback` | Yes | mkfifoat callers use the same adaptor with a FIFO-specific fallback closure |
| `single_component_leaf` | Yes | Identical resolution logic |
| Platform dispatch | **No** | macOS uses `libc::mkfifoat` directly, not `libc::mknodat` |
| Fallback closure body | **No** | FIFO fallback calls `apple_fs::mkfifo` or `rustix mknodat(CWD, ..., Fifo)`, device fallback calls `apple_fs::mknod` or `rustix mknodat(CWD, ..., CharacterDevice/BlockDevice)` |
| Testing | **No** | FIFO tests need no privileges; device tests need root or `--fake-super` |

**Decision**: ship a thin `mkfifoat` raw wrapper alongside the shared
`mknodat` from SEC-MK.b. The adaptor layer is reused. The FIFO-specific
fallback closure is supplied by the call site in the engine crate.

---

## 3. New helper: `mkfifoat`

A new public function in
`crates/fast_io/src/dir_sandbox/at_syscalls.rs`:

### 3.1 API signature

```rust
/// Create a FIFO (named pipe) beneath `dirfd`.
///
/// On Linux, issues `mknodat(dirfd, name, S_IFIFO | perm_bits, 0)`.
/// On macOS 10.13+, issues the native `mkfifoat(dirfd, name, perm_bits)`.
///
/// `name` must not contain an interior NUL byte; callers that pull
/// names from `Path::file_name` cannot trigger this.
///
/// # Errors
///
/// Surfaces the underlying syscall error verbatim. Notable cases:
/// - `EEXIST` when `name` already exists beneath `dirfd`.
/// - `ENOTDIR` when `dirfd` is not a directory.
/// - `EACCES` when the caller lacks write permission on `dirfd`.
/// - `EINVAL` when `name` contains an interior NUL byte.
pub fn mkfifoat(
    dirfd: BorrowedFd<'_>,
    name: &OsStr,
    perm_bits: u32,
) -> io::Result<()>
```

### 3.2 Linux implementation

On Linux, no dedicated `mkfifoat` syscall exists. The standard POSIX
approach is `mknodat` with `S_IFIFO`:

```rust
#[cfg(all(unix, not(any(
    target_os = "ios", target_os = "macos",
    target_os = "tvos", target_os = "watchos",
))))]
pub fn mkfifoat(
    dirfd: BorrowedFd<'_>,
    name: &OsStr,
    perm_bits: u32,
) -> io::Result<()> {
    mknodat(dirfd, name, libc::S_IFIFO as u32 | perm_bits, 0)
}
```

This delegates to the `mknodat` raw wrapper from SEC-MK.b. The `dev`
argument is `0` because FIFOs carry no device number.

### 3.3 macOS implementation

macOS has provided `mkfifoat(2)` since macOS 10.13 (High Sierra,
released 2017). The minimum deployment target for oc-rsync is macOS
11.0, so `mkfifoat` is unconditionally available. Using the native
syscall avoids composing `S_IFIFO | perm_bits` into a combined mode
word and matches the platform idiom:

```rust
#[cfg(any(
    target_os = "ios", target_os = "macos",
    target_os = "tvos", target_os = "watchos",
))]
pub fn mkfifoat(
    dirfd: BorrowedFd<'_>,
    name: &OsStr,
    perm_bits: u32,
) -> io::Result<()> {
    let c_name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;

    // SAFETY:
    // - `dirfd.as_raw_fd()` returns the raw fd of a `BorrowedFd<'_>`
    //   whose lifetime is bound to the borrow and outlives the syscall.
    // - `c_name.as_ptr()` is a valid NUL-terminated C string borrowed
    //   for the duration of the call; the kernel does not retain the
    //   pointer past return.
    // - `perm_bits` is cast to `mode_t` (u16 on macOS); the cast
    //   truncates the upper 16 bits, which is safe because POSIX
    //   permission bits never exceed 0o7777 (12 bits).
    #[allow(unsafe_code)]
    let rc = unsafe {
        libc::mkfifoat(
            dirfd.as_raw_fd(),
            c_name.as_ptr(),
            perm_bits as libc::mode_t,
        )
    };

    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
```

**Note on `libc::mkfifoat` availability**: the `libc` crate exposes
`mkfifoat` on Apple targets. If it is missing from an older `libc`
version, the fallback is `mknodat(dirfd, name, S_IFIFO | perm_bits, 0)`
using the SEC-MK.b wrapper, which is available on macOS 11.0+ via
`libc::mknodat`.

### 3.4 Windows (no-op, cfg-gated)

The `mkfifoat` function is `#[cfg(unix)]`-gated and does not compile
on Windows. The entire `dir_sandbox` module is `#[cfg(unix)]`. On
Windows, the existing no-op stub in `metadata::special::create_fifo_inner`
returns `Ok(())` silently. No sandbox migration is needed.

---

## 4. Sandbox-or-fallback adaptor

mkfifoat reuses the SEC-MK.b `mknodat_via_sandbox_or_fallback` adaptor
with FIFO-specific arguments. No separate `mkfifoat_via_sandbox_or_fallback`
function is needed. The call site composes the adaptor call:

```rust
// At the engine layer (e.g., copy_fifo)
mknodat_via_sandbox_or_fallback(
    context.sandbox(),
    context.dest_dir(),
    relative_path,
    destination,
    libc::S_IFIFO as u32 | perm_bits,
    0, // dev = 0 for FIFOs
    || create_fifo_with_fake_super(destination, metadata, fake_super)
        .map_err(Into::into),
)
```

Alternatively, a thin convenience wrapper can be provided:

```rust
/// Create a FIFO beneath `sandbox` when available, falling back to
/// path-based `mkfifo`/`mknod` otherwise.
///
/// Convenience wrapper over `mknodat_via_sandbox_or_fallback` that
/// composes `S_IFIFO` mode bits and passes `dev = 0`.
pub fn mkfifoat_via_sandbox_or_fallback<F>(
    sandbox: Option<&super::DirSandbox>,
    dest_dir: &Path,
    relative_path: &Path,
    fifo_path: &Path,
    perm_bits: u32,
    fallback: F,
) -> io::Result<()>
where
    F: FnOnce() -> io::Result<()>,
{
    if let Some(sandbox) = sandbox
        && let Some(leaf) = single_component_leaf(dest_dir, relative_path, fifo_path)
    {
        return mkfifoat(sandbox.current_dirfd(), leaf, perm_bits);
    }
    fallback()
}
```

**Recommendation**: ship the convenience wrapper. It eliminates the
`S_IFIFO` mode composition and `dev = 0` boilerplate at every call
site, making intent explicit. On macOS, this wrapper routes through the
native `mkfifoat(2)` syscall rather than `mknodat(S_IFIFO)`, which is
the platform-idiomatic path.

---

## 5. Integration with the receiver pipeline

### 5.1 Current call chain

```
receiver::transfer_ops::apply_entry()
  -> engine::local_copy::executor::special::copy_fifo()
    -> metadata::special::create_fifo_with_fake_super()
      -> create_fifo_inner()
        -> [Linux] rustix::fs::mknodat(CWD, destination, Fifo, mode, makedev(0,0))
        -> [macOS] apple_fs::mkfifo(destination, mode)
        -> [Windows] Ok(())
```

### 5.2 Migrated call chain

```
receiver::transfer_ops::apply_entry()
  -> engine::local_copy::executor::special::copy_fifo()
    -> [sandbox path] mkfifoat_via_sandbox_or_fallback(
           sandbox, dest_dir, relative, destination, perm_bits,
           || create_fifo_with_fake_super(destination, metadata, fake_super)
       )
         -> [single-component leaf + sandbox present]
              mkfifoat(sandbox.current_dirfd(), leaf, perm_bits)
                -> [Linux] libc::mknodat(dirfd, leaf, S_IFIFO | perm, 0)
                -> [macOS] libc::mkfifoat(dirfd, leaf, perm)
         -> [multi-component or no sandbox]
              create_fifo_with_fake_super(destination, metadata, fake_super)
                -> [fake_super=true] create_fake_super_placeholder()
                -> [fake_super=false] create_fifo_inner()
```

### 5.3 Sandbox threading

The `DirSandbox` must be threaded from the receiver into the engine's
copy context. SEC-MK.b section 6 details two approaches:

1. **BorrowedFd threading** - pass `Option<BorrowedFd<'_>>` through
   `metadata::special` signatures.
2. **Closure-based approach** (recommended) - wrap the `metadata::special`
   call in the adaptor closure at the engine layer, keeping `metadata`
   dependency-free of `fast_io`.

This spec follows SEC-MK.b's recommendation: use the closure-based
approach. The engine crate already depends on `fast_io` and has access
to the sandbox. The `metadata` crate's public API remains unchanged.

### 5.4 Socket creation

Unix-domain sockets flow through `create_fifo_inner` on Linux (via
`rustix mknodat(CWD, ..., Socket, ...)`) and through `apple_fs::mknod`
with `S_IFSOCK` on macOS. Socket creation cannot use `mkfifoat` - it
must use `mknodat` with `S_IFSOCK` type bits. The socket path is
covered by SEC-MK.b's `mknodat` wrapper, not by this spec.

The `mkfifoat_via_sandbox_or_fallback` convenience wrapper should
therefore only be used for FIFOs. Socket call sites use
`mknodat_via_sandbox_or_fallback` directly with `S_IFSOCK` mode bits.

---

## 6. Platform behaviour summary

| Platform | Raw helper | Syscall | Fallback |
|----------|-----------|---------|----------|
| Linux (all supported) | `mkfifoat(dirfd, name, perm)` | `libc::mknodat(dirfd, name, S_IFIFO \| perm, 0)` via SEC-MK.b | `rustix::fs::mknodat(CWD, path, Fifo, mode, makedev(0,0))` |
| macOS 11.0+ | `mkfifoat(dirfd, name, perm)` | `libc::mkfifoat(dirfd, name, perm)` | `apple_fs::mkfifo(path, perm)` |
| macOS < 11.0 | Not supported | N/A | Minimum deployment target is 11.0, so this case does not arise |
| Windows | `#[cfg(unix)]` - not compiled | N/A | `create_fifo_inner` returns `Ok(())` |

### 6.1 mode_t truncation on macOS

`libc::mode_t` is `u16` on macOS. The `perm_bits: u32` parameter is
cast via `as libc::mode_t`. This truncates the upper 16 bits, which is
safe because:

- POSIX permission bits are at most 12 bits (`0o7777`).
- No type bits (`S_IFIFO`, etc.) are encoded in `perm_bits` for
  `mkfifoat` - the syscall implies the FIFO type.
- A debug assertion `debug_assert!(perm_bits <= 0o7777)` should be
  added to catch misuse during development.

This matches the existing `mkdirat` and `fchmodat` helpers that perform
the same cast.

---

## 7. Testing strategy

### 7.1 Advantages over device node testing

| Aspect | mkfifoat (this spec) | mknodat for devices (SEC-MK.b) |
|--------|---------------------|-------------------------------|
| Privileges required | None | `CAP_MKNOD` or root |
| CI environment | All runners | Root containers only |
| `--fake-super` needed | No (test real path directly) | Yes (for non-root CI) |
| Verification | `fstatat_nofollow` + `is_fifo()` | `fstatat_nofollow` + `is_char_device()` / `is_block_device()` |

### 7.2 Unit tests (in `crates/fast_io/src/dir_sandbox/tests.rs`)

All tests use `tempfile::TempDir` with `DirSandbox::open_root`. No root
privileges are required.

**Test 1 - mkfifoat creates a FIFO beneath dirfd:**

```rust
#[test]
fn mkfifoat_creates_fifo_under_sandbox() {
    let (_keep, root) = canonical_tempdir();
    let dirfd = secure_open_dir(&root).expect("open root");
    mkfifoat(dirfd.as_fd(), OsStr::new("test.fifo"), 0o644)
        .expect("mkfifoat");

    let meta = fstatat_nofollow(dirfd.as_fd(), OsStr::new("test.fifo"))
        .expect("fstatat");
    assert_eq!(meta.mode() & libc::S_IFMT as u32, libc::S_IFIFO as u32);
    assert!(root.join("test.fifo").exists());
}
```

**Test 2 - mkfifoat returns EEXIST on collision:**

```rust
#[test]
fn mkfifoat_returns_eexist_when_name_exists() {
    let (_keep, root) = canonical_tempdir();
    let dirfd = secure_open_dir(&root).expect("open root");
    std::fs::write(root.join("exists"), b"x").expect("write");

    let err = mkfifoat(dirfd.as_fd(), OsStr::new("exists"), 0o644)
        .expect_err("must fail on existing entry");
    assert_eq!(err.raw_os_error(), Some(libc::EEXIST));
}
```

**Test 3 - mkfifoat preserves permission bits (modulo umask):**

```rust
#[test]
fn mkfifoat_applies_requested_permissions() {
    let (_keep, root) = canonical_tempdir();
    let dirfd = secure_open_dir(&root).expect("open root");
    mkfifoat(dirfd.as_fd(), OsStr::new("perm.fifo"), 0o600)
        .expect("mkfifoat");

    let meta = fstatat_nofollow(dirfd.as_fd(), OsStr::new("perm.fifo"))
        .expect("fstatat");
    let mode = meta.mode() & 0o777;
    // umask may strip bits, but no extra bits should appear
    assert!(mode & 0o066 == 0,
        "mode 0o600 must not grant group/other access, got {mode:o}");
}
```

**Test 4 - adaptor uses sandbox path for single-component leaf:**

```rust
#[test]
fn mkfifoat_via_sandbox_uses_dirfd_for_single_component() {
    let (_keep, root) = canonical_tempdir();
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");
    let fifo_path = root.join("via-sandbox.fifo");
    let leaf = Path::new("via-sandbox.fifo");

    mkfifoat_via_sandbox_or_fallback(
        Some(&sandbox), &root, leaf, &fifo_path, 0o644,
        || panic!("fallback must not be called for single-component leaf"),
    ).expect("sandbox path");

    let meta = std::fs::symlink_metadata(&fifo_path).expect("stat");
    assert!(meta.file_type().is_fifo());
}
```

**Test 5 - adaptor calls fallback for multi-component path:**

```rust
#[test]
fn mkfifoat_via_sandbox_falls_back_for_multi_component() {
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir(root.join("sub")).expect("mkdir");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");
    let fifo_path = root.join("sub").join("deep.fifo");
    let rel = Path::new("sub/deep.fifo");

    let fallback_called = std::sync::atomic::AtomicBool::new(false);
    mkfifoat_via_sandbox_or_fallback(
        Some(&sandbox), &root, rel, &fifo_path, 0o644,
        || {
            fallback_called.store(true, std::sync::atomic::Ordering::Relaxed);
            // Actually create the FIFO via the path-based API for verification
            #[cfg(all(unix, not(target_os = "macos")))]
            rustix::fs::mknodat(
                rustix::fs::CWD, &fifo_path,
                rustix::fs::FileType::Fifo,
                rustix::fs::Mode::from_bits_truncate(0o644),
                rustix::fs::makedev(0, 0),
            ).map_err(|e| std::io::Error::from(e))?;
            Ok(())
        },
    ).expect("fallback path");

    assert!(fallback_called.load(std::sync::atomic::Ordering::Relaxed));
}
```

**Test 6 - adaptor calls fallback when sandbox is None:**

```rust
#[test]
fn mkfifoat_via_sandbox_falls_back_when_sandbox_absent() {
    let (_keep, root) = canonical_tempdir();
    let fifo_path = root.join("no-sandbox.fifo");
    let leaf = Path::new("no-sandbox.fifo");

    let fallback_called = std::sync::atomic::AtomicBool::new(false);
    mkfifoat_via_sandbox_or_fallback(
        None, &root, leaf, &fifo_path, 0o644,
        || {
            fallback_called.store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        },
    ).expect("no-sandbox fallback");

    assert!(fallback_called.load(std::sync::atomic::Ordering::Relaxed));
}
```

### 7.3 TOCTOU resistance test

```rust
#[test]
fn mkfifoat_does_not_follow_symlink_at_leaf() {
    let (_keep, root) = canonical_tempdir();
    let outside = root.join("outside");
    std::fs::create_dir(&outside).expect("mkdir outside");
    symlink(&outside, root.join("escape")).expect("symlink");

    let dirfd = secure_open_dir(&root).expect("open root");
    let err = mkfifoat(dirfd.as_fd(), OsStr::new("escape"), 0o644)
        .expect_err("symlink at leaf must fail");
    assert_eq!(err.raw_os_error(), Some(libc::EEXIST));
    // The symlink target must be unmodified
    assert!(outside.is_dir());
}
```

This test verifies the core security property: a symlink swap at the
leaf between the receiver's "decide to create" moment and the kernel
reaching the inode cannot redirect FIFO creation to an attacker-chosen
directory.

### 7.4 Fallback parity test

```rust
#[test]
fn sandbox_and_fallback_produce_identical_fifo_metadata() {
    let (_keep, root) = canonical_tempdir();
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    // Create via sandbox path
    mkfifoat(sandbox.current_dirfd(), OsStr::new("via-at.fifo"), 0o644)
        .expect("mkfifoat");

    // Create via path-based API
    #[cfg(all(unix, not(target_os = "macos")))]
    rustix::fs::mknodat(
        rustix::fs::CWD, &root.join("via-path.fifo"),
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_bits_truncate(0o644),
        rustix::fs::makedev(0, 0),
    ).expect("path-based mknodat");

    let at_meta = std::fs::symlink_metadata(root.join("via-at.fifo"))
        .expect("stat via-at");
    let path_meta = std::fs::symlink_metadata(root.join("via-path.fifo"))
        .expect("stat via-path");

    // Same file type
    assert!(at_meta.file_type().is_fifo());
    assert!(path_meta.file_type().is_fifo());

    // Same permission bits (both subject to the same umask)
    assert_eq!(
        at_meta.permissions().mode() & 0o777,
        path_meta.permissions().mode() & 0o777,
    );
}
```

### 7.5 Cross-platform compilation

Verify `cargo clippy --workspace --all-targets --all-features` passes
on Linux, macOS, and Windows. The `#[cfg(unix)]` gating on the raw
helper and adaptor must not produce unused-import or dead-code warnings
on any platform.

---

## 8. Migration plan

### Phase 1: Add `mkfifoat` to `at_syscalls.rs`

1. Implement `mkfifoat(dirfd, name, perm_bits)` with platform-specific
   `#[cfg]` branches (section 3).
2. Implement `mkfifoat_via_sandbox_or_fallback` convenience wrapper
   (section 4).
3. Re-export from `crates/fast_io/src/dir_sandbox/mod.rs`.
4. Add unit tests (section 7.2 through 7.4).

**Dependency**: SEC-MK.b must land first (provides `mknodat` raw
wrapper used by the Linux `mkfifoat` implementation).

### Phase 2: Wire sandbox into engine copy context

This is shared work with SEC-MK.b (section 6.2 of that spec). Once
the `CopyContext` or its builder carries `Option<&DirSandbox>`:

1. `copy_fifo` wraps `create_fifo_with_fake_super` in
   `mkfifoat_via_sandbox_or_fallback` with a fallback closure.
2. The `--fake-super` branch is unaffected - the fallback closure
   invokes `create_fifo_with_fake_super` which handles placeholder
   creation internally.

### Phase 3: Integration verification

1. Run full nextest suite in CI to verify no regressions.
2. Run interop tests against upstream rsync 3.0.9, 3.1.3, 3.4.1, 3.4.2
   with `--specials` enabled to verify FIFO round-trip fidelity.
3. Verify macOS CI passes (exercises `libc::mkfifoat` code path).

---

## 9. Files changed

| File | Change |
|------|--------|
| `crates/fast_io/src/dir_sandbox/at_syscalls.rs` | Add `mkfifoat`, `mkfifoat_via_sandbox_or_fallback` |
| `crates/fast_io/src/dir_sandbox/mod.rs` | Re-export `mkfifoat`, `mkfifoat_via_sandbox_or_fallback` |
| `crates/fast_io/src/dir_sandbox/tests.rs` | Add FIFO-specific unit tests |
| `crates/engine/src/local_copy/executor/special/` | Wrap `create_fifo_with_fake_super` in sandbox adaptor (Phase 2, shared with SEC-MK.b) |

Files **not** changed:

| File | Reason |
|------|--------|
| `crates/metadata/src/special.rs` | Public API unchanged; closure-based approach keeps sandbox awareness in the engine crate |
| Windows platform code | No-op stubs remain; `dir_sandbox` is `#[cfg(unix)]` |

---

## 10. Risks and mitigations

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| `libc` crate missing `mkfifoat` binding for Apple targets | Low - `mkfifoat` is in libc 0.2.x for macOS | If missing, fall back to `mknodat(dirfd, name, S_IFIFO \| perm, 0)` using the SEC-MK.b wrapper, which is available on macOS 11.0+ |
| `mode_t` truncation on macOS (`u32` to `u16`) drops perm bits | Very low - perm bits fit in 12 bits | Add `debug_assert!(perm_bits <= 0o7777)` at the `mkfifoat` entry point |
| Fallback closure captures reference across adaptor dispatch | Low | The closure borrows only stack-local references; no lifetime extension needed |
| SEC-MK.b not landing first, blocking Linux `mkfifoat` | Medium | If SEC-MK.b is delayed, `mkfifoat` can be self-contained by inlining the `mknodat` raw call with `libc::mknodat` directly |

---

## 11. Upstream rsync reference

- `syscall.c:163-210` - `do_mknod()` implementation. Lines 183-185
  show the `!MKNOD_CREATES_FIFOS` branch that routes FIFOs through
  `mkfifo(2)` instead of `mknod(2)`.
- `syscall.c:170-176` - `--fake-super` placeholder substitution
  (`am_root < 0` branch). Creates a regular `0600` file via
  `open(O_WRONLY|O_CREAT|O_TRUNC)`.
- `receiver.c` - FIFO creation during file-list application.

---

## 12. Open questions

1. **Should `mkfifoat` be a separate export or inlined at call sites?**
   This spec recommends a separate export for readability and to
   encapsulate the platform-specific `#[cfg]` branching. If the
   SEC-MK.b `mknodat` raw wrapper is sufficient and call sites are
   few, inlining `mknodat(dirfd, name, S_IFIFO | perm, 0)` at each
   site is also acceptable.

2. **Socket creation scope.** This spec excludes Unix-domain socket
   creation. Upstream rsync creates sockets via `bind(2)` on a
   `PF_UNIX` socket, not via `mknod(S_IFSOCK)`, on platforms where
   `!MKNOD_CREATES_SOCKETS`. The Linux `create_fifo_inner` handles
   sockets via `mknodat(CWD, ..., Socket, ...)`, but upstream's
   `bind(2)` path may be needed for full cross-platform parity. This
   is out of scope for SEC-MK.c and can be addressed in a follow-up.
