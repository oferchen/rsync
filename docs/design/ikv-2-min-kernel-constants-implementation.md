# IKV-2: MIN_KERNEL constants per io_uring opcode dispatch site (implementation spec)

Status: design spec - implementation deferred to a follow-up PR that runs
`cargo build`/`cargo nextest` verification.

Tracking: parent task IKV-2 (#2874). Memory note inline:
`[[project_iouring_kernel_version_floor]]`.

## 1. Scope

IKV-2 attaches a machine-readable minimum-kernel constant to every
`IORING_OP_*` opcode that the `fast_io` crate dispatches. The intent is to
move the per-opcode kernel-floor knowledge that today lives only in the
audit doc (IKV-1) into the source tree as `pub const` values, so that:

- The IKV-3 runtime probe matrix can iterate over a single, authoritative
  table instead of hard-coding numbers per probe site.
- Each opcode dispatch site carries a doc-comment that names the kernel
  floor it depends on, citing the IKV-1 audit.

What this spec does NOT do:

- It does NOT add any runtime gate. The dispatch logic is unchanged. The
  runtime probe matrix that consumes these constants is IKV-3 (#2875).
- It does NOT modify wire behaviour, exit codes, or fallback semantics.
  Every opcode already has a fallback path (see IKV-1, "Effective
  minimum kernel and silent-degradation map").

A follow-up implementation PR will apply the mechanical changes described
in section 5; that PR runs the build and test suite.

## 2. Source of truth

The canonical per-opcode kernel-floor table lives in
[`docs/audit/iouring-opcode-kernel-floor.md`](../audit/iouring-opcode-kernel-floor.md)
(IKV-1, merged via PR #4899). Re-quoted here for self-containment:

| Opcode | Numeric | Min kernel | Existing constant | Fallback |
|---|---|---|---|---|
| `IORING_OP_NOP` | 0 | 5.1 | none | test scaffolding only |
| `IORING_OP_FSYNC` | 3 | 5.1 | none | standard `fsync(2)` |
| `IORING_OP_READ_FIXED` | 4 | 5.1 | none | plain `IORING_OP_READ` |
| `IORING_OP_WRITE_FIXED` | 5 | 5.1 | none | plain `IORING_OP_WRITE` |
| `IORING_OP_POLL_ADD` | 6 | 5.1 | none | blocking writes outside io_uring |
| `IORING_OP_ASYNC_CANCEL` (user-data) | 14 | 5.5 | `ASYNC_CANCEL_MIN_KERNEL` | no-op |
| `IORING_OP_ASYNC_CANCEL` (fd-targeted) | 14 | 5.19 | `ASYNC_CANCEL_FD_MIN_KERNEL` | user-data cancel |
| `IORING_OP_LINK_TIMEOUT` | 15 | 5.5 | none | best-effort, tolerated absent |
| `IORING_OP_STATX` | 21 | 5.11 | `STATX_MIN_KERNEL` | libc `statx`/`stat` |
| `IORING_OP_READ` | 22 | 5.6 | `MIN_KERNEL_VERSION` (hard floor) | standard `read(2)` |
| `IORING_OP_WRITE` | 23 | 5.6 | `MIN_KERNEL_VERSION` (hard floor) | standard `write(2)` |
| `IORING_OP_SEND` | 26 | 5.6 | `MIN_KERNEL_VERSION` (hard floor) | blocking `send(2)` |
| `IORING_OP_RECV` | 27 | 5.6 | `MIN_KERNEL_VERSION` (hard floor) | blocking `recv(2)` |
| `IORING_OP_RENAMEAT` | 35 | 5.11 | none (probe-only) | libc `renameat2` |
| `IORING_OP_LINKAT` | 39 | 5.15 | `LINKAT_MIN_KERNEL` | libc `linkat` |
| `IORING_OP_SEND_ZC` | 44 | 6.0 | none (cargo-feature gated) | `IORING_OP_SEND` |
| `IORING_REGISTER_PBUF_RING` | 22 (reg) | 5.19 | `MIN_PBUF_RING_KERNEL` | provide-buffers / plain `READ` |
| `IORING_UNREGISTER_PBUF_RING` | 23 (reg) | 5.19 | `MIN_PBUF_RING_KERNEL` | provide-buffers / plain `READ` |

Tier summary (from IKV-1):

| Tier | Opcodes |
|---|---|
| 5.1 | `NOP`, `FSYNC`, `READ_FIXED`, `WRITE_FIXED`, `POLL_ADD` |
| 5.5 | `ASYNC_CANCEL` (user-data), `LINK_TIMEOUT` |
| 5.6 | `READ`, `WRITE`, `SEND`, `RECV` (oc-rsync hard floor) |
| 5.11 | `STATX`, `RENAMEAT` |
| 5.15 | `LINKAT` |
| 5.19 | `PBUF_RING` register/unregister, `ASYNC_CANCEL` fd-targeted |
| 6.0 | `SEND_ZC` |

Existing kernel-floor constants already live in
`crates/fast_io/src/io_uring_common.rs` (`LINKAT_MIN_KERNEL`,
`STATX_MIN_KERNEL`, `ASYNC_CANCEL_MIN_KERNEL`,
`ASYNC_CANCEL_FD_MIN_KERNEL`),
`crates/fast_io/src/io_uring/config.rs` (`MIN_KERNEL_VERSION = (5, 6)`),
and `crates/fast_io/src/io_uring/buffer_ring/registration.rs`
(`MIN_PBUF_RING_KERNEL`). IKV-2 fills the gaps for opcodes that lack one
(`NOP`, `FSYNC`, `READ_FIXED`, `WRITE_FIXED`, `POLL_ADD`, `LINK_TIMEOUT`,
`READ`, `WRITE`, `SEND`, `RECV`, `RENAMEAT`, `SEND_ZC`) and unifies all
constants behind a single import surface.

## 3. Change-surface inventory

Every io_uring SQE-building dispatch site under
`crates/fast_io/src/io_uring/` (plus the two opcode call sites in
`crates/fast_io/src/copy_file_range.rs`) that IKV-2 must touch. Lines
are relative to current master and were re-verified after the IKV-1
audit; they match the IKV-1 file:line citations.

### 3.1 `IORING_OP_NOP` (5.1)

- `crates/fast_io/src/io_uring/per_thread_ring.rs:181`
  `let entry = opcode::Nop::new().build().user_data(op_id);`
  - Add doc-comment: `/// Requires Linux kernel >= kernel_floor::NOP.`

(The other reference in
`crates/fast_io/src/io_uring/registered_buffers/tests/drop_contract.rs:31`
is test scaffolding; no doc-comment required.)

### 3.2 `IORING_OP_FSYNC` (5.1)

- `crates/fast_io/src/io_uring/file_writer.rs:289`
  `let entry = opcode::Fsync::new(fd).build().user_data(0);`
- `crates/fast_io/src/io_uring/disk_batch.rs:238`
  `let entry = opcode::Fsync::new(fd).build().user_data(0);`
  - Both sites: `/// Requires Linux kernel >= kernel_floor::FSYNC.`

### 3.3 `IORING_OP_READ_FIXED` (5.1)

- `crates/fast_io/src/io_uring/registered_buffers/submit.rs:38`
  `use io_uring::opcode::ReadFixed;` (entry built immediately below)
  - Doc-comment on the SQE build site:
    `/// Requires Linux kernel >= kernel_floor::READ_FIXED.`

### 3.4 `IORING_OP_WRITE_FIXED` (5.1)

- `crates/fast_io/src/io_uring/registered_buffers/submit.rs:173`
  `use io_uring::opcode::WriteFixed;` (entry built immediately below)
  - `/// Requires Linux kernel >= kernel_floor::WRITE_FIXED.`

### 3.5 `IORING_OP_POLL_ADD` (5.1)

- `crates/fast_io/src/io_uring/shared_ring.rs:262`
  `let entry = opcode::PollAdd::new(fd, libc::POLLOUT as u32) ...`
- `crates/fast_io/src/io_uring/cancel.rs:392`
  `let poll_sqe = opcode::PollAdd::new(types::Fd(read_fd.fd), libc::POLLIN as u32) ...`
- `crates/fast_io/src/io_uring/cancel.rs:454`
  `let sqe = opcode::PollAdd::new(types::Fd(read_fd.fd), libc::POLLIN as u32) ...`
- `crates/fast_io/src/io_uring/batching.rs:189`
  `let poll_entry = opcode::PollAdd::new(sqe_fd(fd.0, fixed_fd_slot), pollout_mask) ...`
  - All four sites: `/// Requires Linux kernel >= kernel_floor::POLL_ADD.`

### 3.6 `IORING_OP_ASYNC_CANCEL` (user-data form, 5.5)

- `crates/fast_io/src/io_uring/cancel.rs:160`
  `let entry = opcode::AsyncCancel::new(user_data) ...`
  - `/// Requires Linux kernel >= kernel_floor::ASYNC_CANCEL.`
  - Constant should re-export `ASYNC_CANCEL_MIN_KERNEL` from
    `io_uring_common` for source-of-truth alignment.

### 3.7 `IORING_OP_ASYNC_CANCEL` (fd-targeted form, 5.19)

- `crates/fast_io/src/io_uring/cancel.rs:205`
  `let entry = opcode::AsyncCancel2::new(builder) ...`
  - `/// Requires Linux kernel >= kernel_floor::ASYNC_CANCEL_FD.`
  - Re-export `ASYNC_CANCEL_FD_MIN_KERNEL`.

### 3.8 `IORING_OP_LINK_TIMEOUT` (5.5)

- `crates/fast_io/src/io_uring/batching.rs:194`
  `let timeout_entry = opcode::LinkTimeout::new(timeout as *const types::Timespec) ...`
  - `/// Requires Linux kernel >= kernel_floor::LINK_TIMEOUT.`

### 3.9 `IORING_OP_STATX` (5.11)

- `crates/fast_io/src/io_uring/statx.rs:177`
  `opcode::Statx::new( ...`
- `crates/fast_io/src/io_uring/statx.rs:346`
  `let sqe = opcode::Statx::new( ...`
  - Both sites: `/// Requires Linux kernel >= kernel_floor::STATX.`
  - Re-export `STATX_MIN_KERNEL`.

### 3.10 `IORING_OP_READ` (5.6)

- `crates/fast_io/src/io_uring/file_reader.rs:121`
  `let entry = opcode::Read::new(fd, buf.as_mut_ptr(), to_read as u32) ...`
- `crates/fast_io/src/io_uring/file_reader.rs:213`
  `let entry = opcode::Read::new(fd, ptr, clamped as u32) ...`
- `crates/fast_io/src/io_uring/linked_chain.rs:275`
  `... => opcode::Read::new(types::Fd(fd), ptr, len) ...`
- `crates/fast_io/src/io_uring/linked_chain.rs:318`
  `let read_entry = opcode::Read::new(types::Fd(src_fd), ptr, len) ...`
- `crates/fast_io/src/io_uring/shared_ring.rs:235`
  `let entry = opcode::Read::new(fd, buf.as_mut_ptr(), buf.len() as u32) ...`
- `crates/fast_io/src/copy_file_range.rs:167`
  `... opcode::Read::new(io_uring::types::Fd(src_fd), buf.as_mut_ptr(), want as u32) ...`
  - All six sites: `/// Requires Linux kernel >= kernel_floor::READ.`
  - Constant equals `MIN_KERNEL_VERSION` (the hard floor); this is the
    same value, but expressed per-opcode for symmetry with the others.

### 3.11 `IORING_OP_WRITE` (5.6)

- `crates/fast_io/src/io_uring/file_writer.rs:134`
  `let entry = opcode::Write::new(fd, buf.as_ptr(), buf.len() as u32) ...`
- `crates/fast_io/src/io_uring/batching.rs:90`
  `let entry = opcode::Write::new(fd, data[start + done..].as_ptr(), want as u32) ...`
- `crates/fast_io/src/io_uring/linked_chain.rs:285`
  `... => opcode::Write::new(types::Fd(fd), ptr, len) ...`
- `crates/fast_io/src/io_uring/linked_chain.rs:323`
  `let write_entry = opcode::Write::new(types::Fd(dst_fd), ptr.cast_const(), len) ...`
- `crates/fast_io/src/copy_file_range.rs:198`
  `... opcode::Write::new( ...`
  - All five sites: `/// Requires Linux kernel >= kernel_floor::WRITE.`

### 3.12 `IORING_OP_SEND` (5.6)

- `crates/fast_io/src/io_uring/shared_ring.rs:289`
  `let entry = opcode::Send::new(fd, data.as_ptr(), data.len() as u32) ...`
- `crates/fast_io/src/io_uring/batching.rs:317`
  `let entry = opcode::Send::new( ...`
  - Both sites: `/// Requires Linux kernel >= kernel_floor::SEND.`

### 3.13 `IORING_OP_RECV` (5.6)

- `crates/fast_io/src/io_uring/socket_reader.rs:49`
  `let entry = opcode::Recv::new(fd, self.buffer.as_mut_ptr(), self.buffer_size as u32) ...`
- `crates/fast_io/src/io_uring/socket_reader.rs:90`
  `let entry = opcode::Recv::new(fd, buf.as_mut_ptr(), buf.len() as u32) ...`
  - Both sites: `/// Requires Linux kernel >= kernel_floor::RECV.`

### 3.14 `IORING_OP_RENAMEAT` (5.11)

- `crates/fast_io/src/io_uring/renameat2.rs:142`
  `opcode::RenameAt::new( ...`
  - `/// Requires Linux kernel >= kernel_floor::RENAMEAT.`

### 3.15 `IORING_OP_LINKAT` (5.15)

- `crates/fast_io/src/io_uring/linkat.rs:152`
  `opcode::LinkAt::new( ...`
  - `/// Requires Linux kernel >= kernel_floor::LINKAT.`
  - Re-export `LINKAT_MIN_KERNEL`.

### 3.16 `IORING_OP_SEND_ZC` (6.0)

- `crates/fast_io/src/io_uring/send_zc.rs:150`
  `let entry = opcode::SendZc::new(types::Fd(fd), buf.as_ptr(), buf.len() as u32) ...`
  - `/// Requires Linux kernel >= kernel_floor::SEND_ZC.`
  - Also requires the `iouring-send-zc` cargo feature (existing gate
    in `lib.rs:367`); the kernel-floor constant is orthogonal.

### 3.17 `IORING_REGISTER_PBUF_RING` / `IORING_UNREGISTER_PBUF_RING` (5.19)

- Already covered by `MIN_PBUF_RING_KERNEL` in
  `crates/fast_io/src/io_uring/buffer_ring/registration.rs:14-24`.
- IKV-2 adds a re-export so the central table mentions
  `kernel_floor::PBUF_RING` for completeness.

### 3.18 Inventory totals

- Opcodes covered: 13 SQE opcodes + 1 register opcode = 14 entries.
- Dispatch sites that gain a doc-comment: 27 (the per-site sums above).
- New `pub const` declarations created in `kernel_floor.rs`: 14
  (12 new + 2 re-exports of register-side constants for unified
  access). Existing `*_MIN_KERNEL` constants in `io_uring_common.rs`
  are re-exported, not duplicated.

## 4. Naming convention

Three options were considered:

- **(a) Per-opcode constant adjacent to each dispatch site.** Each
  module declares its own `MIN_KERNEL_<OPCODE>` next to the opcode
  usage. Pros: locality. Cons: duplication when the same opcode is
  dispatched from many modules (`READ` lives in five files); the
  IKV-3 probe matrix would have to import 13 constants from 13 paths.

- **(b) Single table in a new `kernel_floor.rs` module.** All
  per-opcode floors live in
  `crates/fast_io/src/io_uring/kernel_floor.rs` and are imported by
  dispatch sites. Pros: one canonical table mirroring IKV-1's audit;
  one place to update if a backport changes the floor; the IKV-3
  probe matrix imports a single module; the unit test that asserts
  table integrity lives next to the data. Cons: dispatch sites carry
  a doc-comment rather than an inline constant declaration (still
  fine - the constant is named in the comment).

- **(c) Type-level tag (attribute or wrapper newtype).** A
  `#[min_kernel((5, 6))]` proc-macro attribute or
  `KernelGated<MIN_KERNEL_READ, opcode::Read>` wrapper that the
  type system enforces. Pros: type-safe. Cons: drags in a proc-macro
  crate or a wrapper layer over `io_uring::opcode` for a static
  table that never changes at runtime; over-engineered.

**Decision: (b).** A single `pub mod kernel_floor` is the smallest
change that unifies the table, mirrors the IKV-1 audit shape one-to-one,
keeps the IKV-3 consumer trivial, and avoids scattering the same number
across multiple modules. Existing `*_MIN_KERNEL` constants in
`io_uring_common.rs` are re-exported through the new module so callers
have a single import path; the underlying definitions stay where they
are to avoid churn on the cross-platform stub.

## 5. Implementation steps (mechanical)

Each step is independent and applies cleanly without touching wire
behaviour or fallback semantics.

### Step 1 - create `kernel_floor.rs`

Add `crates/fast_io/src/io_uring/kernel_floor.rs` with one
`pub const <OPCODE_NAME>: (u32, u32) = (X, Y);` per entry from section 3
plus re-exports of the existing constants:

```rust
//! Per-opcode minimum-kernel table for the `fast_io` io_uring backend.
//!
//! This is the in-source mirror of the audit table in
//! `docs/audit/iouring-opcode-kernel-floor.md` (IKV-1). Dispatch sites
//! cite these constants in their doc-comments; the IKV-3 runtime probe
//! matrix consumes them programmatically via [`all`].

use crate::io_uring_common::{
    ASYNC_CANCEL_FD_MIN_KERNEL, ASYNC_CANCEL_MIN_KERNEL, LINKAT_MIN_KERNEL,
    STATX_MIN_KERNEL,
};

/// Minimum Linux kernel that supports `IORING_OP_NOP`.
pub const NOP: (u32, u32) = (5, 1);
/// Minimum Linux kernel that supports `IORING_OP_FSYNC`.
pub const FSYNC: (u32, u32) = (5, 1);
/// Minimum Linux kernel that supports `IORING_OP_READ_FIXED`.
pub const READ_FIXED: (u32, u32) = (5, 1);
/// Minimum Linux kernel that supports `IORING_OP_WRITE_FIXED`.
pub const WRITE_FIXED: (u32, u32) = (5, 1);
/// Minimum Linux kernel that supports `IORING_OP_POLL_ADD`.
pub const POLL_ADD: (u32, u32) = (5, 1);
/// Minimum Linux kernel that supports `IORING_OP_ASYNC_CANCEL`
/// (cancel-by-user-data form).
pub const ASYNC_CANCEL: (u32, u32) = ASYNC_CANCEL_MIN_KERNEL;
/// Minimum Linux kernel that supports `IORING_OP_LINK_TIMEOUT`.
pub const LINK_TIMEOUT: (u32, u32) = (5, 5);
/// Minimum Linux kernel that supports `IORING_OP_READ`.
pub const READ: (u32, u32) = (5, 6);
/// Minimum Linux kernel that supports `IORING_OP_WRITE`.
pub const WRITE: (u32, u32) = (5, 6);
/// Minimum Linux kernel that supports `IORING_OP_SEND`.
pub const SEND: (u32, u32) = (5, 6);
/// Minimum Linux kernel that supports `IORING_OP_RECV`.
pub const RECV: (u32, u32) = (5, 6);
/// Minimum Linux kernel that supports `IORING_OP_STATX`.
pub const STATX: (u32, u32) = STATX_MIN_KERNEL;
/// Minimum Linux kernel that supports `IORING_OP_RENAMEAT`.
pub const RENAMEAT: (u32, u32) = (5, 11);
/// Minimum Linux kernel that supports `IORING_OP_LINKAT`.
pub const LINKAT: (u32, u32) = LINKAT_MIN_KERNEL;
/// Minimum Linux kernel that supports the fd-targeted cancel
/// (`AsyncCancel2`) match modes.
pub const ASYNC_CANCEL_FD: (u32, u32) = ASYNC_CANCEL_FD_MIN_KERNEL;
/// Minimum Linux kernel that supports `IORING_REGISTER_PBUF_RING`.
pub const PBUF_RING: (u32, u32) = (5, 19);
/// Minimum Linux kernel that supports `IORING_OP_SEND_ZC`.
pub const SEND_ZC: (u32, u32) = (6, 0);

/// Returns every opcode entry as `(name, (major, minor))`.
///
/// IKV-3's runtime probe matrix iterates over this slice instead of
/// hard-coding the table. New opcodes added to the audit must appear
/// here too.
pub const fn all() -> &'static [(&'static str, (u32, u32))] {
    &[
        ("NOP", NOP),
        ("FSYNC", FSYNC),
        ("READ_FIXED", READ_FIXED),
        ("WRITE_FIXED", WRITE_FIXED),
        ("POLL_ADD", POLL_ADD),
        ("ASYNC_CANCEL", ASYNC_CANCEL),
        ("LINK_TIMEOUT", LINK_TIMEOUT),
        ("READ", READ),
        ("WRITE", WRITE),
        ("SEND", SEND),
        ("RECV", RECV),
        ("STATX", STATX),
        ("RENAMEAT", RENAMEAT),
        ("LINKAT", LINKAT),
        ("ASYNC_CANCEL_FD", ASYNC_CANCEL_FD),
        ("PBUF_RING", PBUF_RING),
        ("SEND_ZC", SEND_ZC),
    ]
}
```

### Step 2 - declare the module

Insert into `crates/fast_io/src/io_uring/mod.rs` (alphabetical position,
after `mod file_writer;`):

```rust
pub mod kernel_floor;
```

The module is implicitly `#[cfg(target_os = "linux")]` because the
entire `io_uring` module already lives behind that gate; non-Linux
builds compile the `io_uring_stub` and never see `kernel_floor`.

### Step 3 - annotate dispatch sites

For each file:line listed in section 3, prepend a single doc-comment
line immediately above the opcode-building statement:

```rust
/// Requires Linux kernel >= kernel_floor::READ.
let entry = opcode::Read::new(fd, buf.as_mut_ptr(), to_read as u32) ...
```

The comment is intentionally one line per site - it is not module-level
documentation, it is a pointer that lets a reader (or grep) jump from
the dispatch site to the kernel-floor table. No `use` statement is
introduced at this step; the constant name appears only in the comment.

### Step 4 - unit test in `kernel_floor.rs`

Add a `#[cfg(test)] mod tests` at the bottom of `kernel_floor.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_non_empty() {
        assert!(!all().is_empty(), "kernel_floor::all() must list at least one opcode");
    }

    #[test]
    fn entries_have_plausible_kernel_versions() {
        // 5.1 is the floor (basic ring); no opcode should claim to need older.
        // 6.x is the present-day ceiling for shipped opcodes.
        for (name, (major, minor)) in all() {
            assert!(*major >= 5, "{name} kernel-floor major version too low: {major}");
            assert!(*major <= 6, "{name} kernel-floor major version too high: {major}");
            if *major == 5 {
                assert!(*minor <= 19, "{name} 5.x minor out of range: {minor}");
            }
        }
    }

    #[test]
    fn known_opcodes_match_io_uring_common() {
        assert_eq!(ASYNC_CANCEL, ASYNC_CANCEL_MIN_KERNEL);
        assert_eq!(ASYNC_CANCEL_FD, ASYNC_CANCEL_FD_MIN_KERNEL);
        assert_eq!(STATX, STATX_MIN_KERNEL);
        assert_eq!(LINKAT, LINKAT_MIN_KERNEL);
    }
}
```

The third test pins the new table to the existing `io_uring_common`
constants so a drift in either side is caught at `cargo nextest` time.

### Step 5 - expose `all()` for IKV-3

`kernel_floor::all()` is already public from Step 1. IKV-3's probe
matrix imports it directly:

```rust
use crate::io_uring::kernel_floor;
for (name, (major, minor)) in kernel_floor::all() { ... }
```

No additional API surface is required.

## 6. Type definition

`(u32, u32)` for `(major, minor)`.

- Matches `uname -r` and the existing `MIN_KERNEL_VERSION`,
  `LINKAT_MIN_KERNEL`, `STATX_MIN_KERNEL`, `ASYNC_CANCEL_MIN_KERNEL`,
  `ASYNC_CANCEL_FD_MIN_KERNEL` declarations in
  `crates/fast_io/src/io_uring_common.rs` and
  `crates/fast_io/src/io_uring/config.rs`.
- Tuple ordering provides arithmetic comparison out of the box:
  `(5, 6) < (5, 11)` works because tuple `Ord` is lexicographic.
- `const` and copy-friendly; no allocation, no `String` runtime cost.
- A struct wrapper (`KernelVersion { major: u32, minor: u32 }`) was
  considered and rejected: it requires impls (`Display`, `Ord`,
  `PartialOrd`, `Debug`) that already exist for tuples, and the
  existing code base uniformly uses `(u32, u32)`. Conformance over
  taste (CLAUDE.md rule 11).

## 7. Acceptance criteria for the IKV-2 implementation PR

The follow-up code PR is complete when:

1. `crates/fast_io/src/io_uring/kernel_floor.rs` exists.
2. The module exports one `pub const` per opcode from section 3
   (currently 17 entries including register-side and fd-targeted
   cancel; existing constants from `io_uring_common.rs` are re-exported
   via `pub const NAME: (u32, u32) = OTHER_NAME;` aliases rather than
   duplicated literals).
3. `pub mod kernel_floor;` appears in
   `crates/fast_io/src/io_uring/mod.rs`.
4. Every dispatch site listed in section 3 carries a one-line
   doc-comment that names the constant
   (`Requires Linux kernel >= kernel_floor::<NAME>.`).
5. `kernel_floor.rs` contains at least three unit tests
   (non-emptiness, plausibility, cross-check against
   `io_uring_common`).
6. `cargo build --release -p fast_io` succeeds on Linux,
   macOS, and Windows (non-Linux builds compile `io_uring_stub`, which
   is untouched).
7. `cargo nextest run -p fast_io --all-features -E
   'test(kernel_floor)'` passes on Linux.
8. `cargo fmt` and `cargo clippy --workspace --all-targets --all-features
   --no-deps -- -D warnings` are clean.

No wire format changes, no runtime gating changes, no fallback path
changes. The IKV-2 implementation PR is purely additive.

## 8. Risk surface

- **Naming collision.** `kernel_floor::READ` could in theory clash with
  `std::io::Read` at a call site that imports both. The module-scoped
  path (`kernel_floor::READ`) avoids it; the spec deliberately does
  NOT add `use kernel_floor::*;` anywhere. Dispatch sites only mention
  the constant in a doc-comment, so there is no import surface at the
  site at all in step 3.
- **Cross-platform compile.** Every constant lives inside
  `crate::io_uring`, which is `#[cfg(target_os = "linux")]`. macOS
  and Windows compile `crate::io_uring_stub` and never see
  `kernel_floor`. The unit tests are also Linux-only, matching the
  existing pattern in `io_uring_common.rs`. The four constants
  re-exported from `io_uring_common` are themselves cross-platform
  (`io_uring_common.rs` is unconditional), so the aliases compile
  cleanly under the Linux gate without dragging in cfg conditionals.
- **Stale data.** If a backport pushes an opcode floor down (e.g.,
  RHEL backports `STATX` to a 5.10 kernel), the constants would
  diverge from reality. Mitigation: the IKV-1 audit
  (`docs/audit/iouring-opcode-kernel-floor.md`) cites the upstream
  UAPI header and the per-opcode commits in `io_uring/opcode.c` as
  the source of truth; `kernel_floor.rs` is the in-source mirror.
  Both update together. IKV-3's runtime probe catches the case where
  a kernel claims unsupported despite meeting the version floor.
- **Doc-comment drift.** A future refactor could move the opcode call
  to a different line and forget to move the doc-comment. The unit
  test in step 4 catches the symptom indirectly (table sanity), but
  there is no AST-level guard that the comment sits adjacent to the
  call. Mitigation: keep the doc-comment short and immediately above
  the SQE-build statement so it is visually obvious during review;
  defer a `#[link_to = "kernel_floor::READ"]` proc-macro to a future
  task if drift is observed.
- **Cargo feature interaction.** `SEND_ZC` is double-gated (kernel
  floor 6.0 AND `iouring-send-zc` cargo feature). The constant is
  always defined; consumers must check the cargo feature separately.
  This is intentional: the kernel-floor constant describes what the
  kernel needs, not what the build configuration enables.

## 9. Cross-references

- IKV-1 audit (`docs/audit/iouring-opcode-kernel-floor.md`,
  PR #4899) - canonical per-opcode kernel-floor inventory; this
  spec mirrors it one-to-one into source.
- IKV-3 (#2875, pending) - runtime probe matrix in
  `fast_io::io_uring::probe`; consumes `kernel_floor::all()`.
- IKV-4 (#2876, PR #4902) - README kernel-tier table; the user-facing
  presentation of the same table.
- IKV-5 (#2877, PR #4907) - man-page per-opcode fallback documentation;
  describes the runtime behaviour below each kernel floor.
- IKV-6 (#2878, PR #4904) - release-notes scaffold for the
  kernel-version-floor series.
- Memory note: `[[project_iouring_kernel_version_floor]]`.
