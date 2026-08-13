# Upstream rsync 3.5.0 path-confinement model

Read-only extraction of the rule set oc-rsync must mirror. Source of truth is
the C at `target/interop/upstream-src/rsync-3.5.0/` — primarily `syscall.c`
(3975 lines, up from 2149 in 3.4.4) plus the `t_secure_relpath.c`,
`t_rename_secure.c`, `t_symlink_secure.c` and `t_chmod_secure.c` unit harnesses,
which state the intended invariants more precisely than the call sites do.

This document is the specification the implementation work is measured against.
It records what upstream *does*, not what oc-rsync currently does.

## 0. Why this is not a single resolver

The single most important structural fact, and the one most easily got wrong by
inference: **upstream has two resolvers with different trust models**, selected
by a mode flag rather than by two separate entry points.

| | Transfer-path resolver | Ownership walk |
|---|---|---|
| Selected by | default (`operator_path_resolve == 0`) | `operator_path_resolve == 1` |
| Symlink policy | refuses **all** symlinks | follows a symlink **owned by uid 0 or the euid**, refuses any other-uid one, at **every** component |
| Boundary | confined beneath the transfer root | may legitimately point outside the tree |
| Trust signal | **location** | **authority (ownership)** |
| Applies to | peer-supplied transfer paths | operator-supplied paths: `--backup-dir`, `--temp-dir`, `--*-dest`, merge files |

`syscall.c:545-552` states the reasoning directly: *"An operator path may
legitimately point outside the tree, so the trust signal is authority
(ownership), not location."*

`operator_path_resolve` is a process-global int set around the relevant
operations by `backup.c` and friends, defaulting to 0. Any oc-rsync design that
models this as one policy, or as two unrelated functions, will diverge.

## 1. When the secure resolver is active at all

`secure_relpath_active()` (`syscall.c:100-114`):

```c
if (symlink_optout_allowed())
        return 0;
if (am_daemon && am_chrooted && module_dirlen)
        return 1;
return !am_chrooted && (am_daemon || !am_sender);
```

Three consequences that materially bound the work:

1. **The sender is excluded.** `!am_sender` — the sender still follows `-L` /
   `--copy-links` symlinks by design. Confinement covers every non-chrooted
   *receiver*, plus the daemon.
2. **A chroot is its own confinement** — `!am_chrooted` — *except* for a daemon
   with an inner-module boundary (`am_daemon && am_chrooted && module_dirlen`),
   because the kernel chroot confines the outer path, not the inner module.
   That exception is the CVE-2026-53793 `/./` inner-module escape.
3. **The opt-out disables the resolver on the receiver side too**, not just the
   sender enumeration. Upstream's comment is explicit that an earlier version
   opted out only the sender and thereby failed to restore the pre-3.4.3
   behaviour the opt-out promises.

## 2. Who may opt out — the rule a client must not be able to reach

`symlink_optout_allowed()` (`syscall.c:121-127`):

```c
if (am_daemon)
        return module_id >= 0 && lp_insecure_links(module_id);
return insecure_links;
```

**For a daemon the opt-out is governed ONLY by the module's `insecure links`
configuration — never by a peer-supplied `--insecure-links`.** A client cannot
disable a daemon's confinement, and upstream additionally *drops a connection*
that sends the flag, so a forwarded flag is structurally inert.

This is a security-critical asymmetry. An implementation that threads a single
`insecure_links` boolean from parsed options through to the resolver, without
distinguishing daemon from non-daemon provenance, hands a client the ability to
turn off the daemon's confinement.

## 3. What the confinement root is

`confinement_root()` (`syscall.c:136-147`):

- **daemon** → `module_dir` / `module_dirlen`
- **non-daemon** → `confine_root` / `confine_rootlen`, i.e. `--confine-root`
- `rootlen <= 1` (root is `/`) → nothing is outside; the check is a no-op

**A daemon never honours `--confine-root`.** Upstream's stated reason: the
module dir is the boundary there, and the option arrives in a peer-supplied
argv, so obeying it could only *loosen* the module.

`--confine-root` exists for a server launched by a wrapper with its own
restricted directory — that is, `rrsync`. This is the hook oc-rsync's restricted-shell
subcommand should use rather than inventing a parallel mechanism.

## 4. The escape check for followed symlinks

Because the ownership walk deliberately *follows* an in-tree symlink owned by
root or the euid, that symlink can redirect the resolved target outside the
root. `abspath_outside_confinement()` (`syscall.c:197`) is what catches the
escape, and it applies to the operator-path family specifically.

Two scoping statements upstream makes explicitly, both worth mirroring verbatim
in behaviour:

- **This is ROOT confinement only.** The daemon exclude/filter list is a
  name-based *visibility* filter, not a physical-path boundary: a symlink whose
  own name is not excluded may still resolve into an excluded in-tree subtree,
  exactly as in stock rsync. The defence for a writable module is
  `munge symlinks`, not this walk.
- **`owner_walk_parent()` leaf-checks too.** The walk resolves the *parent*, so
  the resolved leaf is checked separately (`syscall.c:580-594`) — otherwise a
  symlinked operator path could act on a leaf resolving outside the module in an
  otherwise-served directory. A `snprintf` overflow there returns `ENAMETOOLONG`
  and **fails closed**, never skipping the check.

### The `/proc/self/fd/N` pin case

`rrsync` rewrites a validated option path to `/proc/self/fd/N` so no later
symlink can redirect it. Such a pin is spelled *outside* the root by
construction, so upstream judges it by what it points **at**, not by its
spelling (`fd_pin_tail()`, `is_exact_fd_pin()`).

Two details that are the difference between a defence and a hole:

- Only the **exact** pin entry (`/proc/self/fd/7`, all digits) is resolved. A
  planted name like `.../fd/outside-secret` is not treated as a pin. For a
  pinned *parent* spelled `.../fd/7/<leaf>`, the walk resolves the magic link
  itself and checks the components past it.
- **A pin that cannot be resolved is refused, not waved through** — an
  unreadable pin is exactly the case where the outcome of the open is unknowable.

## 5. Leaf sinks reached through the ownership walk

3.5.0 extended the walk to the backup leaf sinks, because a foreign-owned parent
symlink could otherwise redirect a backup symlink-create or a directory removal
outside the backup tree:

- `do_symlink_at()` (`syscall.c:765`) — backing a symlink into an operator `--backup-dir`
- `do_rmdir_at()` (`syscall.c:1402`) — removing a pre-existing backup directory
- `do_rename_at()` (`syscall.c:1866`), `do_link_at()` (`syscall.c:937`) — each confines its side **independently**
- `robust_rename()`'s cross-filesystem (EXDEV) copy fallback — 3.5.0 confines both the fallback copy **and its source unlink**

Other `do_*_at` wrappers on the same walk: `do_unlink_at`, `do_lchown_at`,
`do_mknod_at`, `do_open_at`, `do_chmod_at`, `do_mkdir_at`, `do_stat_at`,
`do_lstat_at`, `do_utimensat_at`.

## 6. Platform surface

- The walk is per-component `openat(O_PATH|O_NOFOLLOW)`, guarded by
  `#if defined AT_FDCWD && defined O_NOFOLLOW && defined O_DIRECTORY`.
- **Pre-`AT_FDCWD` / no-`O_NOFOLLOW` systems fall back to a plain `open()`** —
  upstream's own comment calls this "best-effort". The confinement is simply
  absent there.
- The refusal errno set widened in 3.5.0 (`rsync.h:438`):

  ```c
  #define NOFOLLOW_HIT_SYMLINK(e) ((e) == ELOOP || (e) == EMLINK || (e) == EFTYPE)
  ```

  `EFTYPE` is new, for the BSDs/macOS. Getting this set wrong turns a *refusal*
  into a generic I/O error, which is an observable difference the upstream tests
  assert on.
- `NEWS.md` states the platform goal: the resolver "follows in-tree directory
  symlinks uniformly on every platform via a single race-free per-component
  `O_NOFOLLOW` walk, so `-K` / `-L` / `-k` and `-R` through an in-tree symlinked
  parent behave the same everywhere."
- `*xattrat` syscalls (Linux 6.13+) with a `/proc/self/fd` compatibility path,
  and patched-libacl `*_at` entry points, carry the same treatment for xattr/ACL
  metadata.

## 7. Refusal observable

Refusal surfaces as `errno = ELOOP` from `owner_walk_parent()` on a confinement
escape. The upstream tests assert on the resulting message and exit code, not
merely on the transfer failing, so an implementation that refuses with a
different observable still fails them.

## 8. Consequences for oc-rsync

Recorded here so the implementation tasks start from the right premises rather
than re-deriving them:

1. **Two policies, one walk.** A single resolver type parameterised by trust
   model (location vs ownership), not two implementations and not one merged
   policy. This is the Dependency-Inversion seam: the ownership policy and the
   opt-out are injected, not branched on inside the walk.
2. **The sender is out of scope.** Confinement covers the receiver and the
   daemon. Applying it to the sender would break `-L` / `--copy-links`.
3. **The daemon opt-out must not be reachable from the wire.** Provenance
   (module config vs peer argv) is part of the type, not an afterthought.
4. **`--confine-root` is daemon-inert** and is the intended mechanism for a
   restricted-shell wrapper.
5. **Fail closed on every uncertainty** — unresolvable pin, path too long,
   unreadable target.
6. Being *stricter* than upstream is still a divergence. oc-rsync currently
   refuses a symlinked daemon destination unconditionally
   (`streams.rs:419`), where 3.5.0 refuses only when a third uid owns the
   symlink. Two upstream tests fail on exactly that.

## References

- `target/interop/upstream-src/rsync-3.5.0/syscall.c`
- `t_secure_relpath.c`, `t_rename_secure.c`, `t_symlink_secure.c`, `t_chmod_secure.c`
- `NEWS.md`, sections "SECURITY FIXES" and "BEHAVIOR CHANGES"
- CVEs bearing directly on this model: CVE-2026-53795 (absolute `--temp-dir` /
  `--link-dest` disabled confinement), CVE-2026-53784 and CVE-2026-53793
  (chroot / inner-module escapes), CVE-2026-53796 (non-daemon receiver chdir),
  CVE-2026-53797 (non-daemon sender per-file open), CVE-2026-53799 (receiver
  ACL/xattr symlink race), CVE-2026-53800 (`--remove-source-files` unlink),
  CVE-2026-53801 (directory-scan enumeration escape).
