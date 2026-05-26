# IUS-8.c.3 - Post-migration documentation update spec

Date: 2026-05-26
Scope: housekeeping - identify and update all project documentation
that references the old `io_uring_stub/` tree architecture, now that
the IUS-7/8 trait migration replaces it with a single
`backend_stub.rs` implementing the `IoUringBackend` trait.
Status: **SPEC DRAFT** - no source changes in this PR.
Predecessor: IUS-8.c.1 (non-Linux stub impl spec), IUS-8.c.2 (stub
deletion plan). Completes the IUS-8.c series.

---

## 1. Architectural state change

### 1.1 Before (pre-IUS-8)

The non-Linux io_uring stub was a 21-file mirror tree at
`crates/fast_io/src/io_uring_stub/` that duplicated every public
function, type, and constant from the Linux `io_uring/` module. Each
file in the Linux tree had a corresponding stub file returning
`Err(Unsupported)`, `false`, or `0`. The stub originally lived in a
single 73 KB file (`io_uring_stub.rs`); a prior refactor split it into
21 files totalling 2,479 LoC (2,070 non-test, 409 test). The `lib.rs`
cfg-gate used `#[path = "io_uring_stub/mod.rs"]` to alias the stub
as the `io_uring` module on non-Linux targets.

Costs of this architecture:
- Every Linux-side API change required a matching stub edit.
- Signature drift between platforms was caught by CI but not prevented
  structurally.
- Review noise: stub diffs often dwarfed the substantive Linux diffs.

### 1.2 After (post-IUS-8)

The `IoUringBackend` trait (defined in `io_uring/backend.rs`) provides
a single interface for all platforms. Two implementations exist:

| Platform | Impl file | Struct | LoC |
|----------|-----------|--------|-----|
| Linux (with `io_uring` feature) | `io_uring/backend_impl.rs` | `LinuxIoUringOpsBackend` | ~600 |
| Non-Linux / Linux without feature | `io_uring/backend_stub.rs` | `StubIoUringBackend` | ~200 |

The `io_uring_stub/` directory (21 files, 2,479 LoC) is deleted. The
`#[path = "io_uring_stub/mod.rs"]` alias in `lib.rs` is replaced by
unconditional `pub mod io_uring;` with cfg gating inside
`io_uring/mod.rs`.

### 1.3 Metrics

| Metric | Before | After | Change |
|--------|--------|-------|--------|
| Stub files | 21 | 1 (`backend_stub.rs`) | -20 files |
| Stub LoC (non-test) | 2,070 | ~200 | -1,870 LoC (~90% reduction) |
| Stub test LoC | 409 | ~50 (in `backend_smoke.rs`) | -359 LoC |
| Total stub LoC | 2,479 | ~250 | -2,229 LoC (12x reduction) |
| Signature drift risk | Manual mirror | Compile-time trait enforcement | Eliminated |
| New method cost | Edit 2 files (Linux + stub) | Edit 1 file (trait); impls get compile error | Halved |

Compile-time improvement: non-Linux builds no longer parse and
type-check 2,479 LoC of stub code. The replacement is ~200 LoC with
trivial method bodies (single `return` per method), reducing both
parse time and monomorphization work.

## 2. Documentation references to update

### 2.1 Project memory note

| File | Current content | Required update |
|------|----------------|-----------------|
| `project_io_uring_stub_size.md` | Describes 73 KB stub duplication problem; recommends trait abstraction | Mark as **RESOLVED by IUS-8**. The trait abstraction was implemented (IUS-7.a/b defined the trait, IUS-8.a authored it, IUS-8.b implemented Linux, IUS-8.c replaced the stub). Update description to note the stub is now `backend_stub.rs` (~200 LoC). |

### 2.2 Source code doc comments (7 references)

These live in `crates/fast_io/` and reference `io_uring_stub` or
`crate::io_uring_stub` in rustdoc comments. Each must be updated to
reference `backend_stub` or the `IoUringBackend` trait:

| File | Line(s) | Current reference | Update to |
|------|---------|-------------------|-----------|
| `src/lib.rs` | 180 | `#[path = "io_uring_stub/mod.rs"]` | Remove path alias; unconditional `pub mod io_uring;` |
| `src/io_uring_common.rs` | 5 | `[crate::io_uring_stub]` | `[crate::io_uring::backend_stub]` or `the non-Linux stub` |
| `src/io_uring_common.rs` | 25 | `crate::io_uring_stub` | `crate::io_uring::backend_stub` |
| `src/io_uring_common.rs` | 533 | `[crate::io_uring_stub]` | `the `StubIoUringBackend` impl` |
| `src/io_uring/buffer_ring/mod.rs` | 57 | `io_uring_stub.rs` | `backend_stub.rs` |
| `src/io_uring/mod.rs` | 29 | `io_uring_stub.rs` | `backend_stub.rs` |
| `src/io_uring/renameat2.rs` | 64 | `[crate::io_uring_stub]` | `StubIoUringBackend` |

### 2.3 Design docs with stale `io_uring_stub` references

These design docs reference the old stub architecture. They are
historical records and should not be retroactively edited, but a
note at the top of each should indicate that the described
architecture was superseded by the IUS-8 trait migration:

| File | References | Action |
|------|------------|--------|
| `docs/design/architecture-rationale.md` | 3 | Add note: "The `io_uring_stub` module described below was replaced by `IoUringBackend` trait + `backend_stub.rs` in the IUS-8 series." |
| `docs/design/codebase-navigation.md` | 2 | Add note or update inline references |
| `docs/design/io-strategy-trait.md` | 3 | Add note referencing IUS-8 as the realized form of the trait idea |
| `docs/design/cli-tunability-flags.md` | 2 | Update file path references |
| `docs/design/iouring-socket-daemon-tcp-readiness.md` | 1 | Update inline reference |
| `docs/design/async-io-uring-impact.md` | 1 | Update inline reference |
| `docs/design/macos-kqueue-fast-io.md` | 1 | Update inline reference |
| `docs/design/io-uring-rayon-composition.md` | 1 | Update inline reference |
| `docs/design/ssh-async-default-linux.md` | 1 | Update inline reference |
| `docs/design/io-uring-bgid-namespace.md` | 1 | Update inline reference |
| `docs/design/sqm-1c-workaround-spec.md` | 1 | Update inline reference |

**Total: 11 design docs with 17 stale references.**

### 2.4 IUS series design docs (historical - no update needed)

The IUS-7.a, IUS-8.a, and IUS-8.c.1 specs reference `io_uring_stub`
in the context of "what we are replacing." These references are
correct in their historical context and should not be modified:

| File | References | Action |
|------|------------|--------|
| `docs/design/ius-7a-trait-surface.md` | 6 | No change (describes the problem being solved) |
| `docs/design/ius-8a-io-uring-backend-trait.md` | 7 | No change (describes the deletion plan) |
| `docs/design/ius-8c1-non-linux-iouring-stub.md` | 14 | No change (describes the replacement) |

### 2.5 README.md

The README references io_uring in general terms (feature flags,
kernel tiers, zero-copy) but does not reference `io_uring_stub`
directly. **No update needed.**

## 3. Verification commands

After all documentation updates are applied, run these commands to
confirm no stale references remain in active (non-historical) docs
and source code:

```sh
# 1. Verify no source code references to the deleted module path
git grep -nE 'io_uring_stub' crates/ tools/ xtask/
# Expected: zero matches

# 2. Verify no remaining #[path = "io_uring_stub/..."] aliases
git grep -nE '#\[path.*io_uring_stub' crates/
# Expected: zero matches

# 3. Verify the stub directory is gone
test -d crates/fast_io/src/io_uring_stub && echo "FAIL: stub dir exists" || echo "PASS: stub dir deleted"

# 4. Verify backend_stub.rs exists and implements IoUringBackend
git grep -l 'impl IoUringBackend for StubIoUringBackend' crates/fast_io/
# Expected: crates/fast_io/src/io_uring/backend_stub.rs

# 5. Verify the trait definition exists
git grep -l 'trait IoUringBackend' crates/fast_io/
# Expected: crates/fast_io/src/io_uring/backend.rs

# 6. Count remaining design doc references (informational)
git grep -c 'io_uring_stub' docs/design/ | grep -v ':0$'
# Expected: only IUS-7.a, IUS-8.a, IUS-8.c.1 (historical specs)

# 7. Verify memory note is updated
grep -l 'RESOLVED' project_io_uring_stub_size.md
# Expected: match (note marked resolved)
```

## 4. Execution order

The documentation updates in this spec should be applied in two
phases:

### Phase 1: Concurrent with IUS-8.c stub deletion PR

Source code doc comment updates (section 2.2) must land in the same
PR that deletes `io_uring_stub/` and adds `backend_stub.rs`, because
the rustdoc links would be broken otherwise.

### Phase 2: Follow-up housekeeping PR

- Memory note update (section 2.1)
- Design doc annotations (section 2.3)
- Verification sweep (section 3)

Phase 2 is non-blocking and can be done any time after the deletion
PR merges.

## 5. Relationship to other memory notes

The following memory notes reference io_uring architecture and remain
accurate after the IUS-8 migration. No updates needed:

| Memory note | Status |
|-------------|--------|
| `project_io_uring_scope_metadata_only.md` | Unchanged - describes scope of io_uring ops (metadata only), not the stub |
| `project_io_uring_shared_ring_bottleneck.md` | Unchanged - describes `Arc<Mutex>` contention on shared ring |
| `project_iouring_kernel_version_floor.md` | Unchanged - describes kernel version requirements |
| `project_iouring_marginal_at_small_bench_scale.md` | Unchanged - describes benchmark findings |
| `project_iouring_send_zc_optin_only.md` | Unchanged - describes SEND_ZC feature gating |
| `project_no_windows_io_uring.md` | Unchanged - describes IOCP as partial alternative |

## 6. Success criteria

- [ ] `project_io_uring_stub_size.md` marked RESOLVED with IUS-8
      cross-references
- [ ] All 7 source code doc comment references updated (section 2.2)
- [ ] Supersession notes added to the 3 most impactful design docs
      (`architecture-rationale.md`, `codebase-navigation.md`,
      `io-strategy-trait.md`)
- [ ] All 7 verification commands pass (section 3)
- [ ] No `io_uring_stub` references remain in `crates/`, `tools/`,
      or `xtask/`

---

**Summary.** The IUS-8 series replaces a 21-file, 2,479-LoC stub
mirror with a single ~200-LoC trait impl. This spec inventories 1
memory note, 7 source code doc comments, and 11 design docs that
reference the old architecture. Source code references must be updated
in the deletion PR; design doc annotations and the memory note update
are a follow-up housekeeping task. Verification commands confirm no
stale references persist.
