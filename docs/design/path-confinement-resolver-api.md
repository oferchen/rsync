# Path-confinement resolver API

Design for the shared per-component resolver (task U350-4c.1). Upstream source of
truth: rsync 3.5.0 `syscall.c`, read directly rather than from the spec summary.

## 1. What the consumer walk settled

The API is designed against a measured consumer set, not an estimate:

| class | sites | disposition |
|---|---|---|
| local-copy, operator-trusted (`engine/local_copy/`) | 149 | no confinement; upstream applies none to a non-daemon transfer |
| receiver dest-tree / commit (`transfer/`) | 58 | confined when the peer names the tail |
| metadata leaf-op on a caller-supplied path | 25 | confined via the resolver, not via threaded arguments |
| sender traversal (`generator/file_list/`) | 16 | **anchor held across the walk** |
| delete path | 3 | already dirfd-anchored, deliberately stronger than upstream |

Two findings from that walk drive the API shape:

- `engine::local_copy` is reached only from `crates/core/src/client/`; `crates/daemon`
  has zero references to it. The local-copy sites are therefore operator-trusted and
  need no confinement. The peer-facing alt-dest code is a *separate* implementation in
  `transfer/src/receiver/transfer/setup/sandbox.rs` and `receiver/quick_check.rs`.
- The sender traversal performs `link_stat`/`readlink_stat` per directory entry
  relative to an anchor it has already entered. A one-shot `resolve(path) -> fd` API
  would force a full per-component re-walk for every entry.

## 2. Upstream mechanism

`struct dirstack` (`syscall.c:2803-2811`):

```c
struct dirstack {
        int fds[DS_MAXDEPTH];   /* fds[0] = anchor (borrowed); fds[top] = current dir */
        int top;
        char abspath[MAXPATHLEN];
};
```

Three properties matter for the API:

1. **`fds[0]` is borrowed.** `secure_walk_at` does not close the anchor; the caller
   owns it. A resolver that took ownership would break every caller that reuses a
   held module-root fd.
2. **`abspath` is seeded only when the anchor path is absolute** (`syscall.c:2989-2991`).
   Unseeded means the exclude-aware refusal is disabled and, in upstream's words,
   "non-daemon callers pay nothing". The trust policy is *data*, not a branch.
3. **Depth and symlink hops are bounded** - `DS_MAXDEPTH` is 1024, and `hops` is a
   single budget shared across the whole walk, passed by pointer so a symlink target
   that itself contains symlinks draws from the same allowance.

`ds_descend` (`syscall.c:2891-2965`) is the per-component step:

- `.` is a no-op; `..` pops to the held parent fd and returns `ELOOP` at `top == 0`
  rather than rising above the anchor.
- otherwise `openat(cur, part, O_RDONLY|O_DIRECTORY|O_NOFOLLOW)`.
- on success it pushes the fd, extends `abspath`, then refuses with `ELOOP` if
  `abspath_outside_confinement()` says the resolved path left the module.
- on `ENOTDIR` or a NOFOLLOW hit it probes with `readlinkat`. An **absolute** target
  is refused (`ELOOP`); a **relative** target is followed by re-walking it, so an
  in-tree symlink resolves and its target may itself contain `..`.
- `EMFILE`/`ENFILE` gets a one-shot warning, because holding one dirfd per component
  can exhaust descriptors where a plain `open` would not.

## 3. The central constraint: resolve masks cannot express this

oc currently resolves with `openat2` and `RESOLVE_NO_SYMLINKS`
(`crates/fast_io/src/secure_dir.rs:153`). That cannot implement the contract above,
for a reason that is not a matter of picking a different mask:

- `abspath_outside_confinement` consults **module filter state per component**. The
  kernel cannot evaluate it. Any design that pushes resolution into `openat2` loses
  the exclude-aware refusal entirely.
- `RESOLVE_NO_SYMLINKS` refuses every symlink; upstream **follows relative in-tree
  symlinks**. Building refuse-all would break a plain local `oc-rsync -a src/ dst/`
  where `dst/sub` is a symlink.

`RESOLVE_BENEATH` alone is closer on the symlink question, but it still cannot carry
the per-component exclude check. The manual walk is therefore required, and `openat2`
is at best an optimisation for the sub-case with no confinement root - not the
mechanism. This supersedes the "refuse-all" framing in the original 4a spec.

## 4. Proposed API

oc already has the stack. `DirSandbox` (`crates/fast_io/src/dir_sandbox/mod.rs`)
provides `open_root`, `current_dirfd`, `root_dirfd`, `enter`, `exit`, `depth`,
`lstat_at`, `unlinkat_at`. The design extends that type rather than introducing a
parallel one.

### 4.1 Trust policy as injected data

```rust
/// How a walk treats components it did not itself name.
pub struct ConfinePolicy {
    /// Absolute path of the anchor. `None` disables exclude-aware refusal, so a
    /// non-daemon caller pays nothing. Mirrors upstream's unseeded `ds.abspath`.
    anchor_abspath: Option<PathBuf>,
    /// Consulted per descended component when `anchor_abspath` is set.
    exclude: Option<Arc<dyn ConfinementOracle>>,
    /// Shared symlink-hop budget for one walk.
    hops: u32,
    /// Maximum retained depth.
    max_depth: usize,
}
```

`ConfinePolicy::operator_trusted()` yields the unseeded policy - no oracle, no
abspath tracking. `ConfinePolicy::module(root, oracle)` yields the confined one.
The activation predicate (task 600) becomes a choice of constructor at the session
boundary, not a branch inside each call site. That is what makes the two alt-dest
implementations expressible in one resolver: same operation, different policy.

### 4.2 Retained anchor handle

```rust
impl DirSandbox {
    /// Borrow an existing anchor without taking ownership. Mirrors
    /// `secure_relative_open_at`, which does not close `anchor_fd`.
    pub fn borrow_anchor(anchor: BorrowedFd<'_>, policy: ConfinePolicy) -> io::Result<Self>;

    /// One per-component step: `.`, `..`, a real subdirectory, or a followed
    /// relative symlink. Mirrors `ds_descend`.
    pub fn descend(&mut self, comp: &OsStr) -> Result<(), ConfineError>;

    /// Resolve `relpath` beneath the current position and open its leaf.
    pub fn open_leaf(&mut self, relpath: &Path, how: LeafOpen) -> Result<OwnedFd, ConfineError>;
}
```

The traversal consumers keep one `DirSandbox` per directory and call `descend`/`exit`
around the entry loop, so a sender walk over N entries costs N `openat` calls rather
than N full re-walks. This is the anchor-lifetime requirement, and task 640 reaches
it independently: upstream prefers `dup(module_dirfd)` because re-traversal as the
dropped-privilege uid `EACCES`es under a non-traversable parent.

### 4.3 Error model

`ConfineError` must distinguish refusal from absence, because the caller's response
differs: a refusal is fatal and reportable, a missing component is often a normal
`ENOENT`. Upstream collapses both into errno and relies on the caller; oc should
keep them separate at the type level and map to upstream's errno only at the
syscall boundary, so the wire-visible behaviour is unchanged.

Distinguished cases: `Escape` (`..` above anchor, absolute symlink target, or
outside-confinement), `HopBudgetExhausted`, `DepthExceeded`, `NotFound`,
`NotADirectory`, `Io`.

## 5. Explicitly out of scope

- **`MUTATE_UNLINK` is not touched.** oc's delete path is dirfd-anchored ungated,
  deliberately stronger than upstream (tasks 470/606). The resolver must not weaken
  it to fit a uniform signature.
- **No anchor threading through operation signatures.** Upstream passes
  `basedir = NULL` at every `do_chmod_at`/`do_lchown_at`/utimes site and lets
  `secure_relative_open` substitute the module root itself. Confinement comes from
  the resolver knowing the root, not from N callers passing it correctly.
- **No path-keyed caching in front of the resolver.** A cached result is reused
  without re-resolving, which defeats per-component confinement. If a stat cache is
  ever wired, it must key on a resolved fd, not a `PathBuf`.

## 6. Cross-platform

The NOFOLLOW-hit errno differs by platform - upstream's `NOFOLLOW_HIT_SYMLINK` covers
`ELOOP` on Linux, `EMLINK` on FreeBSD, `EFTYPE` on NetBSD/OpenBSD - and macOS/BSD
evaluate `O_DIRECTORY` before `O_NOFOLLOW`, yielding `ENOTDIR`. The probe must treat
all of these as "may be a symlink, fall through to `readlinkat`", exactly as upstream
does. Task 613 owns the macOS/BSD arm; Windows is a separate decision (task 614).

## 7. Test plan

The resolver's unit suite (task 602) gates any wiring. Each case below is a refusal
or an acceptance that a mask-based implementation would get wrong:

1. relative in-tree symlink is **followed** (the case refuse-all breaks)
2. absolute symlink target is **refused** even when it would land beneath the anchor
3. `..` at `top == 0` is refused; `..` below it pops to the held parent
4. a symlink target containing `..` is walked, not string-collapsed
5. hop budget is shared across nested symlinks, not per-component
6. depth ceiling refuses rather than truncating
7. exclude oracle refuses a component that a symlink redirected into a hidden subtree
8. operator-trusted policy performs no oracle calls at all
9. anchor fd is still open and usable after the sandbox is dropped

Cross-implementation (task 612): the same scenarios against the real 3.5.0 binary,
since a shared wrong belief about upstream is exactly what this epic keeps finding.

## 8. Open questions

1. **`ConfinementOracle` ownership.** The exclude check needs module filter state.
   Does it live in `filters` and get injected, or does `fast_io` grow a trait the
   daemon implements? The latter keeps `fast_io` dependency-free; the former avoids
   a trait object in a hot loop.
2. **Descriptor pressure.** Holding one dirfd per component is a real cost upstream
   warns about explicitly. oc's traversal is parallel in places, multiplying it. This
   wants a measurement before the ceiling is fixed, not an arbitrary constant.
