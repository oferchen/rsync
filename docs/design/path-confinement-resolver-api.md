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
  has zero references to it. Those sites are operator-trusted and need no confinement.
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

oc resolves the non-anchored root with `openat2` and `RESOLVE_NO_SYMLINKS`
(`crates/fast_io/src/secure_dir.rs:153`). That cannot implement the contract above,
for a reason that is not a matter of picking a different mask:

- `abspath_outside_confinement` consults **module filter state per component**. The
  kernel cannot evaluate it. Any design that pushes resolution into `openat2` loses
  the exclude-aware refusal entirely.
- `RESOLVE_NO_SYMLINKS` refuses every symlink; upstream **follows relative in-tree
  symlinks**. Building refuse-all would break a plain local `oc-rsync -a src/ dst/`
  where `dst/sub` is a symlink.

`RESOLVE_BENEATH` alone is closer on the symlink question - and is in fact what the
anchored peer-tail walk already uses - but it still cannot carry the per-component
exclude check. The manual walk is therefore required, and `openat2` is at best an
optimisation for the sub-case with no confinement root, not the mechanism. This
supersedes the "refuse-all" framing in the original 4a spec.

## 4. Proposed API

oc already has the stack. `DirSandbox` (`crates/fast_io/src/dir_sandbox/mod.rs`)
provides `open_root`, `open_dest_anchor`, `current_dirfd`, `root_dirfd`, `enter`,
`exit`, `depth`, `lstat_at`, `unlinkat_at`. The design extends that type rather than
introducing a parallel one.

### 4.1 Trust policy as injected data

```rust
/// Consulted per descended component when the walk is confined.
pub trait ConfinementOracle {
    /// True when `abspath` has left the served tree.
    fn outside_confinement(&self, abspath: &Path) -> bool;
}

/// Zero-sized oracle for operator-trusted walks. Monomorphises away entirely.
pub struct NoExclude;

/// How a walk treats components it did not itself name.
pub struct ConfinePolicy<O: ConfinementOracle = NoExclude> {
    /// Absolute path of the anchor. `None` disables exclude-aware refusal, so a
    /// non-daemon caller pays nothing. Mirrors upstream's unseeded `ds.abspath`.
    anchor_abspath: Option<PathBuf>,
    exclude: Option<O>,
    /// Shared symlink-hop budget for one walk.
    hops: u32,
    /// Maximum retained depth.
    max_depth: usize,
}
```

The oracle is a **generic parameter, not a trait object**: `fast_io` defines the
trait and stays dependency-free, the daemon supplies the implementation, and the
per-component loop monomorphises with no dynamic dispatch. `ConfinePolicy::<NoExclude>`
compiles the check out of an operator-trusted walk completely.

`ConfinePolicy::operator_trusted()` yields the unseeded policy. `ConfinePolicy::module()`
yields the confined one. The activation predicate (task 600) becomes a choice of
constructor at the session boundary rather than a branch inside each call site.

#### Worked example: the alt-dest family, which exists twice

`--link-dest` / `--copy-dest` / `--compare-dest` is the case that makes injected
policy necessary rather than merely tidy. The same operation is implemented in two
places under two different trust models:

| implementation | who names the path | policy |
|---|---|---|
| `engine/src/local_copy/executor/reference.rs` | the **operator**, on the local-copy path | `operator_trusted()` - no abspath seed, no oracle |
| `transfer/src/receiver/transfer/setup/sandbox.rs`, `receiver/quick_check.rs` | the **peer**, under a daemon | `module()` - seeded abspath, oracle consulted per component |

A branch inside the resolver would have to ask "am I a daemon?" at a layer that has
no business knowing. A boolean parameter would push the same question onto every
caller, where it is one forgotten argument away from being wrong. Passing the policy
as a value lets both implementations call the *same* walk and differ only in what
they constructed at the session boundary.

This is also why the confinement work must touch both: fixing only the
`engine/local_copy` sites and calling CVE-2026-53795 closed would leave the
peer-facing half unconfined (tasks 608, 609, 604).

### 4.2 Retained anchor handle

```rust
impl DirSandbox {
    /// Borrow an existing anchor without taking ownership. Mirrors
    /// `secure_relative_open_at`, which does not close `anchor_fd`.
    pub fn borrow_anchor<O: ConfinementOracle>(
        anchor: BorrowedFd<'_>,
        policy: ConfinePolicy<O>,
    ) -> io::Result<Self>;

    /// One per-component step: `.`, `..`, a real subdirectory, or a followed
    /// relative symlink. Mirrors `ds_descend`.
    pub fn descend(&mut self, comp: &OsStr) -> Result<(), ConfineError>;

    /// Resolve `relpath` beneath the current position and open its leaf.
    pub fn open_leaf(&mut self, relpath: &Path, how: LeafOpen) -> Result<OwnedFd, ConfineError>;
}
```

Traversal consumers keep one `DirSandbox` per directory and call `descend`/`exit`
around the entry loop, so a sender walk over N entries costs N `openat` calls rather
than N full re-walks. Task 640 reaches the same requirement independently: upstream
prefers `dup(module_dirfd)` because re-traversal as the dropped-privilege uid
`EACCES`es under a non-traversable parent.

### 4.3 Error model

`ConfineError` must distinguish refusal from absence, because the caller's response
differs: a refusal is fatal and reportable, a missing component is often a normal
`ENOENT`. Upstream collapses both into errno; oc should keep them separate at the
type level and map to upstream's errno only at the syscall boundary, so wire-visible
behaviour is unchanged.

Distinguished cases: `Escape` (`..` above anchor, absolute symlink target, or
outside-confinement), `HopBudgetExhausted`, `DepthExceeded`, `NotFound`,
`NotADirectory`, `Io`.

⚠ A refusal must reach the operator naming the **component and the cause**, not a
downstream protocol symptom. Measured 2026-08-14: on a daemon push into an escaping
symlink, upstream reports `change_dir#3 "sub" (in m) failed: Invalid cross-device
link (18)` and exits 3, while oc exits 23 with `multiplexed frame truncated`. Both
correctly refuse the escape; only one says why. Task 666.

## 5. The cache contract

The resolver's contract is **not** "resolve this path". It is:

> **Resolve this path, and no consumer may substitute a previously observed result
> for it.**

Stating it as a contract rather than as a ban on one mechanism matters, because a
ban on `HashMap<PathBuf, Metadata>` is satisfiable by any number of other things
with the same hazard - a memoised `stat` helper, a directory listing captured once
and indexed later, a `PathBuf`-keyed negative-existence set. Each would silently
defeat per-component confinement for every consumer that hits it, because the
confinement decision lives in the *act of resolving*, not in the value returned.

Any cache that survives across a resolution must therefore key on a **resolved
handle** (an fd, or an anchor plus a single component) rather than on a path string.
`crates/flist`'s `BatchedStatCache` (`HashMap<PathBuf, Arc<fs::Metadata>>`) is the
concrete instance to watch: it is currently unreached, and wiring it without
re-keying would breach this contract (task 656).

## 6. Explicitly out of scope

- **The unlink path is not touched.** oc's delete path is dirfd-anchored ungated,
  deliberately stronger than upstream (tasks 470/606). The resolver must not weaken
  it to fit a uniform signature.
- **No anchor threading through operation signatures.** Upstream passes
  `basedir = NULL` at every `do_chmod_at`/`do_lchown_at`/utimes site and lets
  `secure_relative_open` substitute the module root itself. Confinement comes from
  the resolver knowing the root, not from N callers passing it correctly.

## 7. Cross-platform

The NOFOLLOW-hit errno differs by platform - upstream's `NOFOLLOW_HIT_SYMLINK` covers
`ELOOP` on Linux, `EMLINK` on FreeBSD, `EFTYPE` on NetBSD/OpenBSD - and macOS/BSD
evaluate `O_DIRECTORY` before `O_NOFOLLOW`, yielding `ENOTDIR`. The probe must treat
all of these as "may be a symlink, fall through to `readlinkat`", exactly as upstream
does. Task 613 owns the macOS/BSD arm; Windows is a separate decision (task 614).

## 8. Test plan

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
since a shared wrong belief about upstream is what this epic keeps finding.

## 9. Deferred with an owner

**Descriptor pressure (task 663).** Holding one dirfd per component is a cost
upstream warns about in code, emitting a one-shot `FWARNING` naming `ulimit -n`.
That warning is operator-visible output and falls under the output-fidelity
contract, so it must be mirrored rather than invented. oc's traversal is parallel
where upstream's is not, so the peak is components x concurrent walks - which wants
a measurement against `getrlimit`, not a constant.
